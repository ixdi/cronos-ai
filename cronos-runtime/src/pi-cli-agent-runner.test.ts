import { chmod, mkdtemp, mkdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test } from "bun:test";
import type { AgentPane, AgentRunInput, TaskWorkspace } from "./contracts";
import { PiCliAgentRunner } from "./pi-cli-agent-runner";

async function makeFixture(piScript: string) {
  const root = await mkdtemp(join(tmpdir(), "cronos-pi-runner-"));
  const bin = join(root, "bin");
  await mkdir(bin);
  const herdr = join(bin, "herdr");
  const pi = join(root, "fake-pi");
  await Bun.write(herdr, `#!/bin/sh\nif [ "$1" = pane ] && [ "$2" = run ]; then\n  shift 3\n  /bin/sh -c "$1" >/dev/null 2>&1 &\n  exit 0\nfi\nif [ "$1" = pane ] && [ "$2" = send-keys ]; then\n  touch .fake-cancelled\n  exit 0\nfi\nexit 2\n`);
  await Bun.write(pi, piScript);
  await chmod(herdr, 0o755);
  await chmod(pi, 0o755);
  const git = Bun.spawnSync({ cmd: ["git", "init", "-b", "main", root], stdout: "pipe", stderr: "pipe" });
  if (git.exitCode !== 0) throw new Error(new TextDecoder().decode(git.stderr));
  for (const args of [
    ["config", "user.email", "cronos-test@example.invalid"],
    ["config", "user.name", "Cronos test"],
  ]) {
    const result = Bun.spawnSync({ cmd: ["git", "-C", root, ...args], stdout: "pipe", stderr: "pipe" });
    if (result.exitCode !== 0) throw new Error(new TextDecoder().decode(result.stderr));
  }
  await Bun.write(join(root, "README.md"), "base\n");
  Bun.spawnSync({ cmd: ["git", "-C", root, "add", "README.md"] });
  const commit = Bun.spawnSync({ cmd: ["git", "-C", root, "commit", "-m", "initial"], stdout: "pipe", stderr: "pipe" });
  if (commit.exitCode !== 0) throw new Error(new TextDecoder().decode(commit.stderr));

  const workspace: TaskWorkspace = { id: "workspace-test", taskId: "task-test", rootPath: root };
  const pane: AgentPane = { id: "pane-test", workspaceId: workspace.id };
  const input: AgentRunInput = {
    taskId: workspace.taskId,
    role: "triage",
    prompt: "Classify this; touch PWNED\nwith a quoted 'value'",
    workspace,
    pane,
    signal: new AbortController().signal,
    onEvent: () => {},
  };
  const runner = new PiCliAgentRunner({
    chatModel: "provider/chat-model",
    herdrBinary: herdr,
    piBinary: "./fake-pi",
    timeoutMs: 5_000,
    pollIntervalMs: 10,
  });
  return { root, input, runner, cleanup: () => rm(root, { recursive: true, force: true }) };
}

const successPiScript = `#!/bin/sh
last=
for arg do last="$arg"; done
printf '%s' "$last" > prompt-captured.txt
printf '%s\\n' "$*" > args-captured.txt
printf '%s\\n' 'created by fake Pi' > result.txt
cat <<'JSON'
{"type":"session","version":3,"id":"test","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/workspace"}
{"type":"agent_start"}
{"type":"tool_execution_start","toolCallId":"tool-1","toolName":"codemode","args":{}}
{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"Jev classification completed"}],"stopReason":"stop"}}
{"type":"agent_settled"}
JSON
`;

test("runs Pi JSON mode in the task pane and enables Jev classification for triage", async () => {
  const fixture = await makeFixture(successPiScript);
  try {
    const events: string[] = [];
    const result = await fixture.runner.run({
      ...fixture.input,
      onEvent: (event) => events.push(`${event.type}:${event.message}`),
    });

    expect(result.summary).toBe("Jev classification completed");
    expect(result.changedFiles).toContain("result.txt");
    expect(await Bun.file(join(fixture.root, "prompt-captured.txt")).text()).toBe(fixture.input.prompt);
    const args = await Bun.file(join(fixture.root, "args-captured.txt")).text();
    expect(args).toContain("provider/chat-model");
    expect(args).toContain("read,codemode");
    expect(args).toContain("typesafe");
    expect(args).toContain("jev-latest");
    expect(await Bun.file(join(fixture.root, "PWNED")).exists()).toBe(false);
    expect(events).toContain("started:Starting Pi triage run");
    expect(events).toContain("progress:Pi started tool: codemode");
    expect(events.at(-1)).toBe("completed:Jev classification completed");
    expect((await Array.fromAsync(new Bun.Glob(".git/cronos-agent-output/.cronos-agent-*").scan({ cwd: fixture.root }))).length).toBe(0);
  } finally {
    await fixture.cleanup();
  }
});

test("uses the configured chat model and coding tools for implementation", async () => {
  const fixture = await makeFixture(successPiScript);
  try {
    await fixture.runner.run({ ...fixture.input, role: "implementation", prompt: "Implement the change" });
    const args = await Bun.file(join(fixture.root, "args-captured.txt")).text();
    expect(args).toContain("provider/chat-model");
    expect(args).toContain("read,bash,edit,write");
    expect(args).not.toContain("codemode");
    expect(args).not.toContain("jev-latest");
  } finally {
    await fixture.cleanup();
  }
});

test("limits review and verification roles to read-only tools plus command execution", async () => {
  const fixture = await makeFixture(successPiScript);
  try {
    for (const role of ["code-review", "verification"] as const) {
      await fixture.runner.run({ ...fixture.input, role, prompt: `${role} the change` });
      const args = await Bun.file(join(fixture.root, "args-captured.txt")).text();
      expect(args).toContain("read,bash");
      expect(args).not.toContain("edit");
      expect(args).not.toContain("write");
    }
  } finally {
    await fixture.cleanup();
  }
});

test("interrupts the pane process when the run is cancelled", async () => {
  const fixture = await makeFixture(`#!/bin/sh
printf '%s\\n' '{"type":"agent_start"}'
while [ ! -f .fake-cancelled ]; do sleep 0.01; done
exit 130
`);
  try {
    const controller = new AbortController();
    const run = fixture.runner.run({ ...fixture.input, signal: controller.signal });
    await Bun.sleep(100);
    controller.abort();
    await expect(run).rejects.toHaveProperty("name", "AbortError");
    expect(await Bun.file(join(fixture.root, ".fake-cancelled")).exists()).toBe(true);
  } finally {
    await fixture.cleanup();
  }
});

test("rejects a Pi stream that settles with an assistant error", async () => {
  const fixture = await makeFixture(`#!/bin/sh
cat <<'JSON'
{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"failed"}],"stopReason":"error"}}
{"type":"agent_settled"}
JSON
`);
  try {
    const events: string[] = [];
    await expect(fixture.runner.run({
      ...fixture.input,
      onEvent: (event) => events.push(`${event.type}:${event.message}`),
    })).rejects.toThrow("Pi run failed (error)");
    expect(events).toContain("failed:Pi agent run failed");
  } finally {
    await fixture.cleanup();
  }
});
