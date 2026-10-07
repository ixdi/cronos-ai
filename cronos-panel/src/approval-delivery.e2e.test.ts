import { expect, test } from "bun:test";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { processNextTask } from "cronos-core/task-workflow";
import { createIntakeServer } from "cronos-core/intake-server";
import { GitHubPullRequestService } from "cronos-core/github-delivery";
import { openStorageDatabase } from "cronos-storage/database";
import type {
  AgentPane,
  AgentRole,
  AgentRunInput,
  AgentRunResult,
  AgentRunner,
  CreateWorkspaceInput,
  TaskWorkspace,
  WorkspaceProvider,
} from "cronos-runtime/contracts";
import { executePanelCli, type WorkflowDependenciesFactory } from "./cli";

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

class LocalWorkspaceProvider implements WorkspaceProvider {
  private paneSequence = 0;
  created: TaskWorkspace | undefined;
  closed: string[] = [];

  constructor(private readonly runtimeRoot: string, private readonly repositoryPath: string) {}

  async createWorkspace(input: CreateWorkspaceInput): Promise<TaskWorkspace> {
    const workspace: TaskWorkspace = {
      id: `workspace-${input.taskId}`,
      taskId: input.taskId,
      rootPath: join(this.runtimeRoot, `worktree-${input.taskId}`),
    };
    await run("git", ["clone", "--no-hardlinks", this.repositoryPath, workspace.rootPath]);
    this.created = workspace;
    return workspace;
  }

  async restoreWorkspace(workspace: TaskWorkspace): Promise<TaskWorkspace> { return workspace; }
  async suspendWorkspace(_workspace: TaskWorkspace): Promise<void> {}

  async openPane(workspace: TaskWorkspace, role: AgentRole): Promise<AgentPane> {
    this.paneSequence += 1;
    return { id: `pane-${role}-${this.paneSequence}`, workspaceId: workspace.id };
  }

  async closePane(_pane: AgentPane): Promise<void> {}

  async closeWorkspace(workspace: TaskWorkspace): Promise<void> {
    this.closed.push(workspace.id);
    await rm(workspace.rootPath, { recursive: true, force: true });
  }
}

class LocalAgentRunner implements AgentRunner {
  readonly roles: AgentRole[] = [];

  async run(input: AgentRunInput): Promise<AgentRunResult> {
    this.roles.push(input.role);
    input.onEvent({ type: "started", message: `${input.role} started` });
    if (input.role === "implementation") {
      await mkdir(join(input.workspace.rootPath, "src"), { recursive: true });
      await writeFile(join(input.workspace.rootPath, "src", "feature.ts"), "export const feature = true;\n");
      return { summary: "Added a small feature", changedFiles: ["src/feature.ts"] };
    }
    if (input.role === "documentation") {
      await mkdir(join(input.workspace.rootPath, "docs"), { recursive: true });
      await writeFile(join(input.workspace.rootPath, "docs", "feature.md"), "The feature is available.\n");
      return { summary: "Documented the feature", changedFiles: ["docs/feature.md"] };
    }
    if (input.role === "triage") {
      return { summary: JSON.stringify({ route: "auto_resolvable", reason: "Small bounded change" }), changedFiles: [] };
    }
    if (input.role === "code-review" || input.role === "verification") {
      return { summary: JSON.stringify({ status: "passed", summary: `${input.role} passed` }), changedFiles: [] };
    }
    throw new Error(`Unexpected role ${input.role}`);
  }
}

test("local intake through human approval creates and records a pull request", async () => {
  const directory = await mkdtemp(join(tmpdir(), "cronos-approval-e2e-"));
  const repositoryPath = join(directory, "repo");
  const remotePath = join(directory, "remote.git");
  const runtimeRoot = join(directory, "runtime");
  const databasePath = join(directory, "cronos.sqlite");
  await mkdir(repositoryPath);
  await mkdir(runtimeRoot);
  await run("git", ["init", "--initial-branch=main"], repositoryPath);
  await run("git", ["config", "user.name", "Fixture"], repositoryPath);
  await run("git", ["config", "user.email", "fixture@example.test"], repositoryPath);
  await writeFile(join(repositoryPath, "README.md"), "Cronos fixture\n");
  await writeFile(join(repositoryPath, ".gitignore"), ".env\nignored.tmp\n");
  await run("git", ["add", "--all"], repositoryPath);
  await run("git", ["commit", "-m", "fixture base"], repositoryPath);
  await run("git", ["init", "--bare", "--initial-branch=main", remotePath]);

  const db = await openStorageDatabase(databasePath);
  const workspaceProvider = new LocalWorkspaceProvider(runtimeRoot, repositoryPath);
  const agentRunner = new LocalAgentRunner();
  const intake = createIntakeServer(db, 0);
  const apiCalls: Array<{ owner: string; repo: string; head: string; base: string }> = [];
  const tokenProvider = {
    async getInstallationToken() { return "test-installation-token"; },
    invalidateToken() {},
  };
  const delivery = new GitHubPullRequestService({
    repositoryPath,
    runtimeRoot,
    owner: "fixture-owner",
    repository: "fixture-repo",
    baseBranch: "main",
    tokenProvider,
    pushBranch: async (stagingPath, branch) => {
      await run("git", ["-C", stagingPath, "remote", "set-url", "origin", remotePath]);
      await run("git", ["-C", stagingPath, "push", "origin", branch]);
    },
    octokitFactory: () => ({
      rest: {
        pulls: {
          create: async (request) => {
            apiCalls.push(request);
            return { data: { number: 19, html_url: "https://github.com/fixture-owner/fixture-repo/pull/19" } };
          },
        },
      },
    }),
  });

  try {
    const response = await fetch(`http://127.0.0.1:${intake.port}/tasks`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ description: "Add a small feature" }),
    });
    expect(response.status).toBe(201);
    const queued = await response.json() as { id: string };

    const initial = await processNextTask({
      db,
      workspaceProvider,
      agentRunner,
      repositoryPath,
      baseBranch: "main",
    });
    expect(initial.status).toBe("review_pending");
    expect(agentRunner.roles).toEqual(["triage", "implementation", "code-review", "verification"]);

    const workflowDependencies: WorkflowDependenciesFactory = () => ({
      workspaceProvider,
      agentRunner,
      repositoryPath,
      baseBranch: "main",
      pullRequestCreator: delivery,
    });
    const approved = await executePanelCli(
      ["--db", databasePath, "approve", queued.id],
      {},
      workflowDependencies,
    );
    expect(approved.exitCode).toBe(0);
    expect(JSON.parse(approved.output ?? "{}")).toMatchObject({
      status: "delivered",
      pullRequest: {
        number: 19,
        url: "https://github.com/fixture-owner/fixture-repo/pull/19",
      },
    });
    expect(agentRunner.roles.at(-1)).toBe("documentation");
    expect(apiCalls).toHaveLength(1);
    expect(apiCalls[0]).toMatchObject({
      owner: "fixture-owner",
      repo: "fixture-repo",
      base: "main",
    });
    const branch = apiCalls[0]!.head;
    expect(await run("git", [`--git-dir=${remotePath}`, "show", `${branch}:src/feature.ts`])).toContain("feature = true");
    expect(await run("git", [`--git-dir=${remotePath}`, "show", `${branch}:docs/feature.md`])).toContain("feature is available");
    expect(workspaceProvider.closed).toEqual([`workspace-${queued.id}`]);

    const inspected = await executePanelCli(["--db", databasePath, "show", queued.id]);
    expect(inspected.exitCode).toBe(0);
    expect(JSON.parse(inspected.output ?? "{}")).toMatchObject({
      status: "delivered",
      workflow: {
        status: "completed",
        currentStage: "delivered",
        pullRequest: {
          number: 19,
          url: "https://github.com/fixture-owner/fixture-repo/pull/19",
          branch,
        },
      },
    });
    expect(db.query<{ status: string }, [string]>("SELECT status FROM tasks WHERE id = ?").get(queued.id)?.status)
      .toBe("delivered");
    expect(apiCalls).toHaveLength(1);
  } finally {
    intake.stop(true);
    db.close(true);
    await rm(directory, { recursive: true, force: true });
  }
});
