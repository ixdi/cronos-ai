import { expect, test } from "bun:test";
import { chmod, mkdir, mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { HerdrWorkspaceProvider } from "./herdr-workspace-provider";
import { PiCliAgentRunner } from "./pi-cli-agent-runner";
import type { AgentPane, TaskWorkspace } from "./contracts";

const integrationTest = Bun.env.CRONOS_RUNTIME_INTEGRATION === "1" ? test : test.skip;

function shellQuote(value: string): string {
  return `'${value.replaceAll("'", "'\\''")}'`;
}

async function run(command: string, args: string[], cwd?: string): Promise<string> {
  const process = Bun.spawn({ cmd: [command, ...args], cwd, stdout: "pipe", stderr: "pipe" });
  const [stdout, stderr, exitCode] = await Promise.all([
    new Response(process.stdout).text(),
    new Response(process.stderr).text(),
    process.exited,
  ]);
  if (exitCode !== 0) throw new Error(`${command} failed: ${stderr}`);
  return stdout.trim();
}

integrationTest("isolates a git worktree in Docker and manages Herdr panes on demand", async () => {
  const temporaryRoot = await mkdtemp(join(tmpdir(), "cronos-herdr-integration-"));
  const repositoryPath = join(temporaryRoot, "repository");
  const runtimeRoot = join(temporaryRoot, "runtime");
  await mkdir(repositoryPath);

  let workspace: TaskWorkspace | undefined;
  let pane: AgentPane | undefined;
  let provider: HerdrWorkspaceProvider | undefined;
  const testSecretConfigured = Bun.env.CRONOS_RUNTIME_TEST_SECRET === "cronos-integration-fixture";

  try {
    await run("git", ["init", "-b", "main", repositoryPath]);
    await run("git", ["-C", repositoryPath, "config", "user.email", "cronos-test@example.invalid"]);
    await run("git", ["-C", repositoryPath, "config", "user.name", "Cronos test"]);
    await Bun.write(join(repositoryPath, "README.md"), "base repository\n");
    await run("git", ["-C", repositoryPath, "add", "README.md"]);
    await run("git", ["-C", repositoryPath, "commit", "-m", "initial commit"]);

    provider = new HerdrWorkspaceProvider({
      runtimeRoot,
      runtimeImage: Bun.env.CRONOS_RUNTIME_TEST_IMAGE ?? "python:3.12-alpine3.22",
      runtimeEnv: testSecretConfigured ? ["CRONOS_RUNTIME_TEST_SECRET"] : [],
    });
    workspace = await provider.createWorkspace({
      taskId: "integration-task",
      repositoryPath,
      baseBranch: "main",
    });

    expect(await Bun.file(join(workspace.rootPath, "README.md")).text()).toBe("base repository\n");
    const commonGitDir = await run("git", ["-C", repositoryPath, "rev-parse", "--path-format=absolute", "--git-common-dir"]);
    const worktreeGitDir = await run("git", ["-C", workspace.rootPath, "rev-parse", "--path-format=absolute", "--git-dir"]);
    const commonWriteProbe = join(commonGitDir, "cronos-agent-write-probe");
    const worktreeWriteProbe = join(worktreeGitDir, "cronos-agent-write-probe");
    pane = await provider.openPane(workspace, "implementation");
    const mountCheck = [
      `if touch ${shellQuote(commonWriteProbe)} 2>/dev/null; then rm -f ${shellQuote(commonWriteProbe)}; printf writable > /workspace/common-git-check.txt; else printf readonly > /workspace/common-git-check.txt; fi`,
      `if touch ${shellQuote(worktreeWriteProbe)} 2>/dev/null; then rm -f ${shellQuote(worktreeWriteProbe)}; printf writable > /workspace/worktree-git-check.txt; else printf readonly > /workspace/worktree-git-check.txt; fi`,
    ].join("; ");
    await run("herdr", [
      "pane", "run", pane.id,
      `printf isolated > /workspace/container-created.txt; ${mountCheck}${testSecretConfigured ? `; test "$CRONOS_RUNTIME_TEST_SECRET" = cronos-integration-fixture && printf available > /workspace/runtime-env-check.txt` : ""}`,
    ]);

    const outputPath = join(workspace.rootPath, "container-created.txt");
    const commonCheckPath = join(workspace.rootPath, "common-git-check.txt");
    const worktreeCheckPath = join(workspace.rootPath, "worktree-git-check.txt");
    const envCheckPath = join(workspace.rootPath, "runtime-env-check.txt");
    for (let attempt = 0; attempt < 40; attempt += 1) {
      const outputExists = await Bun.file(outputPath).exists();
      const mountsChecked = await Bun.file(commonCheckPath).exists() && await Bun.file(worktreeCheckPath).exists();
      const envExists = !testSecretConfigured || await Bun.file(envCheckPath).exists();
      if (outputExists && mountsChecked && envExists) break;
      await Bun.sleep(100);
    }
    expect(await Bun.file(outputPath).text()).toBe("isolated");
    expect(await Bun.file(commonCheckPath).text()).toBe("readonly");
    expect(await Bun.file(worktreeCheckPath).text()).toBe("writable");
    expect(await Bun.file(commonWriteProbe).exists()).toBe(false);
    expect(await Bun.file(worktreeWriteProbe).exists()).toBe(false);
    if (testSecretConfigured) expect(await Bun.file(envCheckPath).text()).toBe("available");
    expect(await Bun.file(join(repositoryPath, "container-created.txt")).exists()).toBe(false);

    const fixturePiPath = join(workspace.rootPath, ".pi-fixture-bin", "pi");
    await mkdir(join(workspace.rootPath, ".pi-fixture-bin"));
    await Bun.write(fixturePiPath, `#!/bin/sh\nreadme=$(cat README.md)\nprintf 'read: %s\\nagent changed task workspace\\n' "$readme" > agent-created.txt\ncat <<'JSON'\n{"type":"agent_start"}\n{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"Pi smoke task completed"}],"stopReason":"stop"}}\n{"type":"agent_settled"}\nJSON\n`);
    await chmod(fixturePiPath, 0o755);
    const agentEvents: string[] = [];
    const agentResult = await new PiCliAgentRunner({
      chatModel: "integration/fake-chat-model",
      piBinary: "./.pi-fixture-bin/pi",
    }).run({
      taskId: workspace.taskId,
      role: "implementation",
      prompt: "Create the integration smoke file",
      workspace,
      pane,
      signal: new AbortController().signal,
      onEvent: (event) => agentEvents.push(event.type),
    });
    expect(agentResult.summary).toBe("Pi smoke task completed");
    expect(agentResult.changedFiles).toContain("agent-created.txt");
    expect(await Bun.file(join(workspace.rootPath, "agent-created.txt")).text()).toBe("read: base repository\nagent changed task workspace\n");
    expect(agentEvents).toEqual(["started", "completed"]);
    expect(await Bun.file(join(repositoryPath, "agent-created.txt")).exists()).toBe(false);

    await provider.closePane(pane);
    pane = undefined;
    const persistedWorkspace = { ...workspace };
    await provider.suspendWorkspace(workspace);
    expect(await Bun.file(outputPath).text()).toBe("isolated");
    workspace = await provider.restoreWorkspace(persistedWorkspace, repositoryPath);
    expect(workspace.id).toBe(persistedWorkspace.id);
    expect(workspace.rootPath).toBe(persistedWorkspace.rootPath);
    pane = await provider.openPane(workspace, "implementation");
    await run("herdr", ["pane", "run", pane.id, "printf restored > /workspace/restored-runtime.txt"]);
    for (let attempt = 0; attempt < 40 && !await Bun.file(join(workspace.rootPath, "restored-runtime.txt")).exists(); attempt += 1) {
      await Bun.sleep(100);
    }
    expect(await Bun.file(join(workspace.rootPath, "restored-runtime.txt")).text()).toBe("restored");

    await provider.closePane(pane);
    pane = undefined;
    await provider.closeWorkspace(workspace);
    expect(await Bun.file(workspace.rootPath).exists()).toBe(false);
    workspace = undefined;
  } finally {
    if (provider && pane) await provider.closePane(pane).catch(() => {});
    if (provider && workspace) await provider.closeWorkspace(workspace).catch(() => {});
    await rm(temporaryRoot, { recursive: true, force: true });
  }
});
