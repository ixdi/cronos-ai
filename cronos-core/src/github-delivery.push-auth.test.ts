import { expect, test } from "bun:test";
import { chmod, mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { TaskWorkspace } from "cronos-runtime/contracts";
import { GitHubPullRequestService } from "./github-delivery";

async function run(command: string, args: string[], cwd?: string): Promise<string> {
  const child = Bun.spawn({ cmd: [command, ...args], cwd, stdout: "pipe", stderr: "pipe" });
  const [stdout, stderr, exitCode] = await Promise.all([
    new Response(child.stdout).text(),
    new Response(child.stderr).text(),
    child.exited,
  ]);
  if (exitCode !== 0) throw new Error(`${command} failed (${exitCode}): ${stderr}`);
  return stdout.trim();
}

function shellQuote(value: string): string {
  return `'${value.replaceAll("'", "'\\''")}'`;
}

test("passes the GitHub token via per-process Git config and not command arguments", async () => {
  const directory = await mkdtemp(join(tmpdir(), "cronos-git-auth-test-"));
  const repositoryPath = join(directory, "repo");
  const runtimeRoot = join(directory, "runtime");
  const workspaceRoot = join(runtimeRoot, "worktree-task-auth");
  const logPath = join(directory, "git-push.log");
  const wrapperPath = join(directory, "git-wrapper.sh");
  await mkdir(repositoryPath);
  await mkdir(runtimeRoot);
  await run("git", ["init", "--initial-branch=main"], repositoryPath);
  await run("git", ["config", "user.name", "Fixture"], repositoryPath);
  await run("git", ["config", "user.email", "fixture@example.test"], repositoryPath);
  await writeFile(join(repositoryPath, "README.md"), "base\n");
  await run("git", ["add", "--all"], repositoryPath);
  await run("git", ["commit", "-m", "base"], repositoryPath);
  await run("git", ["clone", "--no-hardlinks", repositoryPath, workspaceRoot]);
  await writeFile(join(workspaceRoot, "README.md"), "task change\n");

  const realGit = await Bun.which("git");
  if (!realGit) throw new Error("git is required for this test");
  await writeFile(wrapperPath, [
    "#!/bin/sh",
    `REAL_GIT=${shellQuote(realGit)}`,
    `LOG_PATH=${shellQuote(logPath)}`,
    'if [ "$#" -ge 3 ] && [ "$1" = "-C" ] && [ "$3" = "push" ]; then',
    '  {',
    '    printf "count=%s\\n" "$GIT_CONFIG_COUNT"',
    '    printf "key0=%s\\n" "$GIT_CONFIG_KEY_0"',
    '    printf "value0=%s\\n" "$GIT_CONFIG_VALUE_0"',
    '    printf "key1=%s\\n" "$GIT_CONFIG_KEY_1"',
    '    printf "value1=%s\\n" "$GIT_CONFIG_VALUE_1"',
    '    printf "args=%s\\n" "$*"',
    '  } > "$LOG_PATH"',
    "  exit 0",
    "fi",
    'exec "$REAL_GIT" "$@"',
    "",
  ].join("\n"));
  await chmod(wrapperPath, 0o755);

  const workspace: TaskWorkspace = {
    id: "workspace-task-auth",
    taskId: "task-auth",
    rootPath: workspaceRoot,
  };
  try {
    const service = new GitHubPullRequestService({
      repositoryPath,
      runtimeRoot,
      owner: "example-owner",
      repository: "example-repo",
      baseBranch: "main",
      gitBinary: wrapperPath,
      tokenProvider: {
        async getInstallationToken() { return "test-token-that-must-not-be-an-argument"; },
        invalidateToken() {},
      },
      octokitFactory: () => ({
        rest: {
          pulls: {
            create: async () => ({ data: { number: 3, html_url: "https://github.com/example-owner/example-repo/pull/3" } }),
          },
        },
      }),
    });
    await service.createPullRequest({
      taskId: workspace.taskId,
      description: "Test push auth",
      workspace,
      documentationSummary: "Docs",
      verificationSummary: "Passed",
    });

    const log = await Bun.file(logPath).text();
    expect(log).toContain("count=2");
    expect(log).toContain("http.https://github.com/example-owner/example-repo.git.extraheader");
    const encodedCredential = log.match(/value0=AUTHORIZATION: basic ([^\n]+)/)?.[1];
    expect(encodedCredential).toBeDefined();
    expect(Buffer.from(encodedCredential!, "base64").toString()).toBe("x-access-token:test-token-that-must-not-be-an-argument");
    const args = log.split("args=")[1] ?? "";
    expect(args).not.toContain("test-token-that-must-not-be-an-argument");
    expect(args).not.toContain(encodedCredential!);
    expect(log).toContain("key1=credential.helper");
    expect(log).toContain("value1=");
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});
