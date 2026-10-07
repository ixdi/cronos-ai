import { expect, test } from "bun:test";
import { access, mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
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

async function createDeliveryFixture() {
  const directory = await mkdtemp(join(tmpdir(), "cronos-github-delivery-"));
  const repositoryPath = join(directory, "repo");
  const runtimeRoot = join(directory, "runtime");
  const workspaceRoot = join(runtimeRoot, "worktree-task-delivery");
  await mkdir(repositoryPath);
  await mkdir(runtimeRoot);
  await run("git", ["init", "--initial-branch=main"], repositoryPath);
  await run("git", ["config", "user.name", "Fixture"], repositoryPath);
  await run("git", ["config", "user.email", "fixture@example.test"], repositoryPath);
  await writeFile(join(repositoryPath, "README.md"), "base contents\n");
  await writeFile(join(repositoryPath, "remove-me.txt"), "delete me\n");
  await writeFile(join(repositoryPath, ".gitignore"), "ignored.tmp\n");
  await run("git", ["add", "--all"], repositoryPath);
  await run("git", ["commit", "-m", "base"], repositoryPath);
  await run("git", ["clone", "--no-hardlinks", repositoryPath, workspaceRoot]);
  await writeFile(join(workspaceRoot, "README.md"), "task contents\n");
  await rm(join(workspaceRoot, "remove-me.txt"));
  await mkdir(join(workspaceRoot, "src"));
  await writeFile(join(workspaceRoot, "src", "new.ts"), "export const delivered = true;\n");
  await writeFile(join(workspaceRoot, "ignored.tmp"), "must not be committed\n");
  const workspace: TaskWorkspace = {
    id: "workspace-task-delivery",
    taskId: "task-delivery",
    rootPath: workspaceRoot,
  };
  return { directory, repositoryPath, runtimeRoot, workspace };
}

test("creates a host-owned commit, pushes a scoped branch, and opens the configured pull request", async () => {
  const fixture = await createDeliveryFixture();
  const remotePath = join(fixture.directory, "remote.git");
  await run("git", ["init", "--bare", "--initial-branch=main", remotePath]);
  const pushed: Array<{ repositoryPath: string; branch: string; token: string }> = [];
  const requests: Array<Record<string, string>> = [];
  let tokenNumber = 0;
  const tokenProvider = {
    async getInstallationToken() {
      tokenNumber += 1;
      return `token-${tokenNumber}`;
    },
    invalidateToken() {},
  };
  try {
    const service = new GitHubPullRequestService({
      ...fixture,
      owner: "example-owner",
      repository: "cronos-product",
      baseBranch: "main",
      tokenProvider,
      pushBranch: async (repositoryPath, branch, token) => {
        pushed.push({ repositoryPath, branch, token });
        expect(await run("git", ["-C", repositoryPath, "show", `${branch}:README.md`])).toBe("task contents");
        expect(await run("git", ["-C", repositoryPath, "show", `${branch}:src/new.ts`])).toContain("delivered = true");
        expect(await run("git", ["-C", repositoryPath, "show", `${branch}:.gitignore`])).toBe("ignored.tmp");
        const removed = Bun.spawn({ cmd: ["git", "-C", repositoryPath, "cat-file", "-e", `${branch}:remove-me.txt`], stdout: "ignore", stderr: "ignore" });
        expect(await removed.exited).not.toBe(0);
        const ignored = Bun.spawn({ cmd: ["git", "-C", repositoryPath, "cat-file", "-e", `${branch}:ignored.tmp`], stdout: "ignore", stderr: "ignore" });
        expect(await ignored.exited).not.toBe(0);
        await run("git", ["-C", repositoryPath, "remote", "set-url", "origin", remotePath]);
        await run("git", ["-C", repositoryPath, "push", "origin", branch]);
      },
      octokitFactory: (token) => ({
        rest: {
          pulls: {
            create: async (input) => {
              expect(token).toBe("token-1");
              requests.push(input);
              return { data: { number: 37, html_url: "https://github.com/example-owner/cronos-product/pull/37" } };
            },
          },
        },
      }),
    });

    const result = await service.createPullRequest({
      taskId: fixture.workspace.taskId,
      description: "Add delivery support",
      workspace: fixture.workspace,
      documentationSummary: "Documented delivery behavior",
      verificationSummary: "All checks passed",
    });
    expect(result).toMatchObject({
      number: 37,
      url: "https://github.com/example-owner/cronos-product/pull/37",
      title: "Cronos: Add delivery support",
    });
    expect(result.commitSha).toMatch(/^[0-9a-f]{40}$/);
    expect(JSON.stringify(result)).not.toContain("token-1");
    expect(result.branch).toMatch(/^cronos\/task-delivery-[0-9a-f]{10}$/);
    expect(pushed).toHaveLength(1);
    expect(pushed[0]?.token).toBe("token-1");
    expect(requests[0]).toMatchObject({
      owner: "example-owner",
      repo: "cronos-product",
      title: "Cronos: Add delivery support",
      head: result.branch,
      base: "main",
    });
    expect(requests[0]?.body).toContain("Documented delivery behavior");
    expect(requests[0]?.body).toContain("All checks passed");
    expect(requests[0]?.body).toContain("src/new.ts");
    expect(await run("git", [`--git-dir=${remotePath}`, "rev-parse", `refs/heads/${result.branch}`])).toBe(result.commitSha);
    expect(await run("git", [`--git-dir=${remotePath}`, "show", `${result.branch}:README.md`])).toBe("task contents");
    await expect(access(pushed[0]!.repositoryPath)).rejects.toThrow();
  } finally {
    await rm(fixture.directory, { recursive: true, force: true });
  }
});

test("refreshes a rejected push token and rejects unsafe repository paths", async () => {
  const fixture = await createDeliveryFixture();
  let tokenNumber = 0;
  let invalidations = 0;
  let pushes = 0;
  let pullRequestAttempts = 0;
  const apiTokens: string[] = [];
  const tokenProvider = {
    async getInstallationToken() {
      tokenNumber += 1;
      return `refresh-token-${tokenNumber}`;
    },
    invalidateToken() { invalidations += 1; },
  };
  try {
    const service = new GitHubPullRequestService({
      ...fixture,
      owner: "example-owner",
      repository: "cronos-product",
      baseBranch: "main",
      tokenProvider,
      pushBranch: async (_path, _branch, token) => {
        pushes += 1;
        if (pushes === 1) {
          expect(token).toBe("refresh-token-1");
          throw new Error("HTTP 401 authentication failed");
        }
        expect(token).toBe("refresh-token-2");
      },
      octokitFactory: (token) => ({
        rest: {
          pulls: {
            create: async () => {
              apiTokens.push(token);
              pullRequestAttempts += 1;
              if (pullRequestAttempts === 1) {
                throw Object.assign(new Error("Unauthorized"), { status: 401 });
              }
              return { data: { number: 1, html_url: "https://github.com/example-owner/cronos-product/pull/1" } };
            },
          },
        },
      }),
    });
    await service.createPullRequest({
      taskId: fixture.workspace.taskId,
      description: "Retry push",
      workspace: fixture.workspace,
      documentationSummary: "Docs",
      verificationSummary: "Passed",
    });
    expect(pushes).toBe(2);
    expect(invalidations).toBe(2);
    expect(tokenNumber).toBe(3);
    expect(apiTokens).toEqual(["refresh-token-2", "refresh-token-3"]);

    const redactionService = new GitHubPullRequestService({
      ...fixture,
      owner: "example-owner",
      repository: "cronos-product",
      baseBranch: "main",
      tokenProvider: {
        async getInstallationToken() { return "private-installation-token"; },
        invalidateToken() {},
      },
      pushBranch: async (_path, _branch, token) => { throw new Error(`401 ${token}`); },
    });
    let redactedFailure = "";
    try {
      await redactionService.createPullRequest({
        taskId: fixture.workspace.taskId,
        description: "Redaction test",
        workspace: fixture.workspace,
        documentationSummary: "Docs",
        verificationSummary: "Passed",
      });
    } catch (error) {
      redactedFailure = error instanceof Error ? error.message : "";
    }
    expect(redactedFailure).toContain("[REDACTED]");
    expect(redactedFailure).not.toContain("private-installation-token");

    expect(() => new GitHubPullRequestService({
      ...fixture,
      owner: "example-owner",
      repository: "../other",
      baseBranch: "main",
      tokenProvider,
    })).toThrow("valid path segments");
  } finally {
    await rm(fixture.directory, { recursive: true, force: true });
  }
});
