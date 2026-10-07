import { expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { processNextTask, reconcileActiveTasks } from "cronos-core/task-workflow";
import { GitHubPullRequestService } from "cronos-core/github-delivery";
import { openStorageDatabase } from "cronos-storage/database";
import type {
  AgentPane,
  AgentRole,
  AgentRunInput,
  AgentRunResult,
  CreateWorkspaceInput,
  AgentRunner,
  TaskWorkspace,
  WorkspaceProvider,
} from "cronos-runtime/contracts";
import { createDefaultWorkflowDependencies, executePanelCli } from "./cli";

class FakeWorkspaceProvider implements WorkspaceProvider {
  readonly calls: string[] = [];

  async createWorkspace(input: CreateWorkspaceInput): Promise<TaskWorkspace> {
    const workspace = { id: `workspace-${input.taskId}`, taskId: input.taskId, rootPath: `/workspaces/${input.taskId}` };
    this.calls.push(`create:${workspace.id}`);
    return workspace;
  }

  async restoreWorkspace(workspace: TaskWorkspace): Promise<TaskWorkspace> {
    this.calls.push(`restore:${workspace.id}`);
    return workspace;
  }

  async suspendWorkspace(workspace: TaskWorkspace): Promise<void> {
    this.calls.push(`suspend:${workspace.id}`);
  }

  async openPane(workspace: TaskWorkspace, role: AgentRole): Promise<AgentPane> {
    const pane = { id: `pane-${role}`, workspaceId: workspace.id };
    this.calls.push(`open:${role}`);
    return pane;
  }

  async closePane(pane: AgentPane): Promise<void> {
    this.calls.push(`close:${pane.id}`);
  }

  async closeWorkspace(workspace: TaskWorkspace): Promise<void> {
    this.calls.push(`close-workspace:${workspace.id}`);
  }
}

class FakeAgentRunner implements AgentRunner {
  readonly roles: AgentRole[] = [];
  readonly prompts: string[] = [];

  async run(input: AgentRunInput): Promise<AgentRunResult> {
    this.roles.push(input.role);
    this.prompts.push(input.prompt);
    input.onEvent({ type: "started", message: `${input.role} started` });
    const summary = input.role === "triage"
      ? JSON.stringify({ route: "auto_resolvable", reason: "Test route" })
      : input.role === "code-review" || input.role === "verification"
        ? JSON.stringify({ status: "passed", summary: `${input.role} passed` })
        : `${input.role} completed`;
    return { summary, changedFiles: input.role === "implementation" ? ["src/change.ts"] : [] };
  }
}

function insertQueuedTask(db: Awaited<ReturnType<typeof openStorageDatabase>>, id: string): void {
  const timestamp = new Date().toISOString();
  db.query(`
    INSERT INTO tasks (id, description, status, created_at, updated_at)
    VALUES (?, ?, 'queued', ?, ?)
  `).run(id, `Task ${id}`, timestamp, timestamp);
}

test("lists task states and renders workflow review/verification output", async () => {
  const directory = await mkdtemp(join(tmpdir(), "cronos-panel-test-"));
  const databasePath = join(directory, "cronos.sqlite");
  const db = await openStorageDatabase(databasePath);
  const timestamp = "2026-01-01T00:00:00.000Z";
  try {
    const insertTask = db.query(`
      INSERT INTO tasks (id, description, status, created_at, updated_at)
      VALUES (?, ?, ?, ?, ?)
    `);
    insertTask.run("queued-1", "Queued work", "queued", "2026-01-01T00:00:00.000Z", timestamp);
    insertTask.run("review-1", "Task awaiting human review", "review_pending", "2026-01-02T00:00:00.000Z", timestamp);
    insertTask.run("blocked-1", "Unsupported request", "blocked", "2026-01-03T00:00:00.000Z", timestamp);
    insertTask.run("errored-1", "Failed checks", "errored", "2026-01-04T00:00:00.000Z", timestamp);
    insertTask.run("delivered-1", "Already delivered", "delivered", "2026-01-05T00:00:00.000Z", timestamp);

    const insertWorkflow = db.query(`
      INSERT INTO workflows (task_id, status, current_stage, last_error, created_at, updated_at)
      VALUES (?, ?, ?, ?, ?, ?)
    `);
    insertWorkflow.run("review-1", "running", "final_review", null, timestamp, timestamp);
    insertWorkflow.run("blocked-1", "blocked", "blocked", "Needs a specification", timestamp, timestamp);
    insertWorkflow.run("errored-1", "errored", "errored", "Verification failed", timestamp, timestamp);
    insertWorkflow.run("delivered-1", "completed", "delivered", null, timestamp, timestamp);
    db.query(`
      UPDATE workflows
      SET pull_request_number = 7,
          pull_request_url = 'https://github.com/example/repo/pull/7',
          pull_request_branch = 'cronos/delivered-1'
      WHERE task_id = 'delivered-1'
    `).run();

    db.query(`
      INSERT INTO agent_runs (id, task_id, role, status, result, started_at, finished_at, created_at)
      VALUES (?, 'review-1', ?, 'succeeded', ?, ?, ?, ?)
    `).run(
      "run-review",
      "code-review",
      JSON.stringify({ summary: "No blocking issues", changedFiles: ["src/main.ts"], structuredResult: { status: "passed" } }),
      timestamp,
      timestamp,
      timestamp,
    );
    db.query(`
      INSERT INTO agent_runs (id, task_id, role, status, result, started_at, finished_at, created_at)
      VALUES (?, 'review-1', ?, 'succeeded', ?, ?, ?, ?)
    `).run(
      "run-verification",
      "verification",
      JSON.stringify({ summary: "All tests passed", changedFiles: [], structuredResult: { status: "passed" } }),
      timestamp,
      timestamp,
      timestamp,
    );
  } finally {
    db.close(true);
  }

  try {
    const listed = await executePanelCli(["--db", databasePath, "list"]);
    expect(listed.exitCode).toBe(0);
    const tasks = JSON.parse(listed.output ?? "[]") as Array<{
      status: string;
      workflow: { currentStage: string; pullRequest: { number: number; url: string } | null } | null;
    }>;
    expect(tasks.map((task) => task.status)).toEqual([
      "queued", "review_pending", "blocked", "errored", "delivered",
    ]);
    expect(tasks[1]?.workflow?.currentStage).toBe("final_review");
    expect(tasks[0]?.workflow).toBeNull();
    expect(tasks.find((task) => task.status === "delivered")?.workflow?.pullRequest).toEqual({
      number: 7,
      url: "https://github.com/example/repo/pull/7",
      branch: "cronos/delivered-1",
    });

    const filtered = await executePanelCli(["--db", databasePath, "list", "--status", "blocked"]);
    expect(JSON.parse(filtered.output ?? "[]").map((task: { id: string }) => task.id)).toEqual(["blocked-1"]);

    const inspected = await executePanelCli(["--db", databasePath, "show", "review-1"]);
    expect(inspected.exitCode).toBe(0);
    const detail = JSON.parse(inspected.output ?? "{}") as {
      codeReview: { summary: string; changedFiles: string[] };
      verification: { summary: string };
    };
    expect(detail.codeReview).toEqual(expect.objectContaining({
      summary: "No blocking issues",
      changedFiles: ["src/main.ts"],
    }));
    expect(detail.verification.summary).toBe("All tests passed");

    const transitionDb = await openStorageDatabase(databasePath);
    try {
      transitionDb.query("UPDATE tasks SET status = 'interrupted' WHERE id = 'review-1'").run();
      transitionDb.query("UPDATE workflows SET status = 'interrupted', current_stage = 'verification', last_error = 'Restarted' WHERE task_id = 'review-1'").run();
    } finally {
      transitionDb.close(true);
    }
    const interrupted = await executePanelCli(["--db", databasePath, "list", "--status", "interrupted"]);
    expect(JSON.parse(interrupted.output ?? "[]")).toMatchObject([
      { status: "interrupted", workflow: { currentStage: "verification", lastError: "Restarted" } },
    ]);

    const resumeDb = await openStorageDatabase(databasePath);
    try {
      resumeDb.query("UPDATE tasks SET status = 'active' WHERE id = 'review-1'").run();
      resumeDb.query("UPDATE workflows SET status = 'running', current_stage = 'implementation', last_error = NULL WHERE task_id = 'review-1'").run();
    } finally {
      resumeDb.close(true);
    }
    const active = await executePanelCli(["--db", databasePath, "list", "--status", "active"]);
    expect(JSON.parse(active.output ?? "[]")).toMatchObject([
      { status: "active", workflow: { currentStage: "implementation" } },
    ]);

    expect(await executePanelCli(["--db", databasePath, "show", "missing"])).toMatchObject({ exitCode: 1 });
    const controlCharacterError = await executePanelCli(["--db", databasePath, "show", "\u001b[31m-missing"]);
    expect(controlCharacterError.error).toContain("\\u001b");
    expect(controlCharacterError.error).not.toContain("\u001b");
    expect(await executePanelCli(["--db", databasePath, "list", "--status", "bogus"])).toMatchObject({ exitCode: 2 });
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("configures GitHub delivery only for actions that can reach PR creation", async () => {
  const db = await openStorageDatabase(":memory:");
  const env = {
    CRONOS_REPOSITORY_PATH: "/repo",
    CRONOS_RUNTIME_ROOT: "/runtime",
    CRONOS_RUNTIME_IMAGE: "cronos-agent:latest",
    CRONOS_PI_CHAT_MODEL: "provider/model",
    CRONOS_GITHUB_OWNER: "example-owner",
    CRONOS_GITHUB_REPOSITORY: "cronos-product",
    CRONOS_GITHUB_APP_ID: "17",
    CRONOS_GITHUB_INSTALLATION_ID: "23",
    CRONOS_GITHUB_REPOSITORY_ID: "42",
    CRONOS_GITHUB_APP_PRIVATE_KEY: "fake-secret-key",
  };
  try {
    const rejection = createDefaultWorkflowDependencies(db, env, "reject");
    expect(rejection.pullRequestCreator).toBeUndefined();
    const approval = createDefaultWorkflowDependencies(db, env, "approve");
    expect(approval.pullRequestCreator).toBeInstanceOf(GitHubPullRequestService);
    expect(JSON.stringify(approval.pullRequestCreator)).not.toContain("fake-secret-key");
    expect(() => createDefaultWorkflowDependencies(db, { ...env, CRONOS_RUNTIME_ENV: "GITHUB_TOKEN" }, "approve"))
      .toThrow("cannot be forwarded");
  } finally {
    db.close(true);
  }
});

test("shows help without opening a database and accepts the database environment variable", async () => {
  expect(await executePanelCli(["--help"])).toMatchObject({ exitCode: 0, output: expect.stringContaining("review_pending") });
  expect(await executePanelCli(["list"], { CRONOS_DB_PATH: ":memory:" })).toMatchObject({ exitCode: 0, output: "[]" });
  expect(await executePanelCli(["show"])).toMatchObject({ exitCode: 2 });
});

test("explicit reject reworks and re-reviews, then approval advances to delivery", async () => {
  const directory = await mkdtemp(join(tmpdir(), "cronos-panel-review-"));
  const databasePath = join(directory, "cronos.sqlite");
  const db = await openStorageDatabase(databasePath);
  const initialProvider = new FakeWorkspaceProvider();
  const initialRunner = new FakeAgentRunner();
  insertQueuedTask(db, "task-cli-review");
  try {
    const first = await processNextTask({
      db,
      workspaceProvider: initialProvider,
      agentRunner: initialRunner,
      repositoryPath: "/repo",
      baseBranch: "main",
    });
    expect(first.status).toBe("review_pending");
    expect(initialRunner.roles).toEqual(["triage", "implementation", "code-review", "verification"]);
  } finally {
    db.close(true);
  }

  try {
    let dependencyFactoryCalls = 0;
    const forbiddenFactory = () => {
      dependencyFactoryCalls += 1;
      throw new Error("Inspection must not resume or run agents");
    };
    const beforeAction = await executePanelCli(["--db", databasePath, "list"], {}, forbiddenFactory);
    expect(beforeAction.exitCode).toBe(0);
    expect(dependencyFactoryCalls).toBe(0);

    const rejectedProvider = new FakeWorkspaceProvider();
    const rejectedRunner = new FakeAgentRunner();
    const rejected = await executePanelCli(
      ["--db", databasePath, "reject", "task-cli-review", "--feedback", "Handle the empty input case"],
      {},
      () => ({
        workspaceProvider: rejectedProvider,
        agentRunner: rejectedRunner,
        repositoryPath: "/repo",
        baseBranch: "main",
      }),
    );
    expect(rejected.exitCode).toBe(0);
    expect(JSON.parse(rejected.output ?? "{}")).toMatchObject({ status: "review_pending", taskId: "task-cli-review" });
    expect(JSON.parse(rejected.output ?? "{}").workspace).toBeUndefined();
    expect(rejectedRunner.roles).toEqual(["implementation", "code-review", "verification"]);
    expect(rejectedRunner.prompts.some((prompt) => prompt.includes("Handle the empty input case"))).toBe(true);

    const approvedProvider = new FakeWorkspaceProvider();
    const approvedRunner = new FakeAgentRunner();
    const approved = await executePanelCli(
      ["--db", databasePath, "approve", "task-cli-review"],
      {},
      () => ({
        workspaceProvider: approvedProvider,
        agentRunner: approvedRunner,
        repositoryPath: "/repo",
        baseBranch: "main",
      }),
    );
    expect(approved.exitCode).toBe(0);
    expect(JSON.parse(approved.output ?? "{}")).toMatchObject({ status: "ready_for_delivery" });
    expect(approvedRunner.roles).toEqual(["documentation"]);

    const stored = await openStorageDatabase(databasePath);
    try {
      expect(stored.query<{ status: string }, [string]>("SELECT status FROM tasks WHERE id = ?")
        .get("task-cli-review")?.status).toBe("active");
      expect(stored.query<{ current_stage: string }, [string]>("SELECT current_stage FROM workflows WHERE task_id = ?")
        .get("task-cli-review")?.current_stage).toBe("pull_request");
    } finally {
      stored.close(true);
    }
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("resume is an explicit CLI action and inspection leaves interrupted work stopped", async () => {
  const directory = await mkdtemp(join(tmpdir(), "cronos-panel-resume-"));
  const databasePath = join(directory, "cronos.sqlite");
  const db = await openStorageDatabase(databasePath);
  const initialProvider = new FakeWorkspaceProvider();
  const controller = new AbortController();
  let markStarted!: () => void;
  const started = new Promise<void>((resolve) => { markStarted = resolve; });
  const blockedRunner: AgentRunner = {
    run(input): Promise<AgentRunResult> {
      input.onEvent({ type: "started", message: "triage started" });
      markStarted();
      return new Promise<AgentRunResult>((_resolve, reject) => {
        if (input.signal.aborted) {
          reject(new DOMException("Aborted", "AbortError"));
          return;
        }
        input.signal.addEventListener("abort", () => reject(new DOMException("Aborted", "AbortError")), { once: true });
      });
    },
  };
  insertQueuedTask(db, "task-cli-resume");
  const dependencies = {
    db,
    workspaceProvider: initialProvider,
    agentRunner: blockedRunner,
    repositoryPath: "/repo",
    baseBranch: "main",
    signal: controller.signal,
  };

  try {
    const inFlight = processNextTask(dependencies);
    await started;
    const reconciled = await reconcileActiveTasks(dependencies);
    expect(reconciled).toMatchObject([{ taskId: "task-cli-resume", runtimeSuspended: true }]);
    controller.abort();
    expect((await inFlight).status).toBe("resume_failed");
  } finally {
    controller.abort();
    db.close(true);
  }

  try {
    let dependencyFactoryCalls = 0;
    const listing = await executePanelCli(["--db", databasePath, "list", "--status", "interrupted"], {}, () => {
      dependencyFactoryCalls += 1;
      throw new Error("Listing must not resume interrupted work");
    });
    expect(listing.exitCode).toBe(0);
    expect(JSON.parse(listing.output ?? "[]")).toMatchObject([
      { status: "interrupted", workflow: { lastError: "Cronos restarted; the task requires explicit operator resume" } },
    ]);
    expect(dependencyFactoryCalls).toBe(0);

    const restoredProvider = new FakeWorkspaceProvider();
    const resumedRunner = new FakeAgentRunner();
    const resumed = await executePanelCli(
      ["--db", databasePath, "resume", "task-cli-resume"],
      {},
      () => ({
        workspaceProvider: restoredProvider,
        agentRunner: resumedRunner,
        repositoryPath: "/repo",
        baseBranch: "main",
      }),
    );
    expect(resumed.exitCode).toBe(0);
    expect(JSON.parse(resumed.output ?? "{}")).toMatchObject({ status: "review_pending" });
    expect(restoredProvider.calls).toContain("restore:workspace-task-cli-resume");
    expect(resumedRunner.roles).toEqual(["triage", "implementation", "code-review", "verification"]);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});
