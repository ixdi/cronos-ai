import { expect, test } from "bun:test";
import { saveQueueItem } from "cronos-queue/queue";
import type {
  AgentPane,
  AgentRole,
  AgentRunner,
  AgentRunInput,
  AgentRunResult,
  CreateWorkspaceInput,
  TaskWorkspace,
  WorkspaceProvider,
} from "cronos-runtime/contracts";
import { openStorageDatabase } from "cronos-storage/database";
import {
  claimNextTask,
  markTaskInterrupted,
  setWorkflowWorkspace,
} from "cronos-storage/workflow-store";
import {
  processNextTask,
  reconcileActiveTasks,
  resolveTaskReview,
  resumeInterruptedTask,
} from "./task-workflow";

class FakeWorkspaceProvider implements WorkspaceProvider {
  readonly calls: string[] = [];
  failRestore = false;

  async createWorkspace(input: CreateWorkspaceInput): Promise<TaskWorkspace> {
    this.calls.push(`create:${input.taskId}`);
    return { id: `workspace-${input.taskId}`, taskId: input.taskId, rootPath: `/tmp/${input.taskId}` };
  }

  async restoreWorkspace(workspace: TaskWorkspace): Promise<TaskWorkspace> {
    this.calls.push(`restore:${workspace.id}`);
    if (this.failRestore) throw new Error("Workspace runtime unavailable");
    return workspace;
  }

  async suspendWorkspace(workspace: TaskWorkspace): Promise<void> {
    this.calls.push(`suspend:${workspace.id}`);
  }

  async openPane(workspace: TaskWorkspace, role: AgentRole): Promise<AgentPane> {
    this.calls.push(`open:${role}`);
    return { id: `pane-${role}`, workspaceId: workspace.id };
  }

  async closePane(pane: AgentPane): Promise<void> {
    this.calls.push(`close-pane:${pane.id}`);
  }

  async closeWorkspace(workspace: TaskWorkspace): Promise<void> {
    this.calls.push(`close-workspace:${workspace.id}`);
  }
}

class FakeAgentRunner implements AgentRunner {
  readonly roles: AgentRole[] = [];
  readonly prompts: string[] = [];

  constructor(
    private readonly route: string,
    private readonly reason: string,
    private readonly gates: { codeReview?: "passed" | "failed"; verification?: "passed" | "failed" } = {},
  ) {}

  async run(input: AgentRunInput): Promise<AgentRunResult> {
    this.roles.push(input.role);
    this.prompts.push(input.prompt);
    input.onEvent({ type: "started", message: `${input.role} started` });
    input.onEvent({ type: "completed", message: `${input.role} completed` });
    const summary = input.role === "triage"
      ? JSON.stringify({ route: this.route, reason: this.reason })
      : input.role === "code-review" || input.role === "verification"
        ? JSON.stringify({
          status: input.role === "code-review" ? this.gates.codeReview ?? "passed" : this.gates.verification ?? "passed",
          summary: `${input.role} ${input.role === "code-review" ? this.gates.codeReview ?? "passed" : this.gates.verification ?? "passed"}`,
        })
        : `${input.role} completed`;
    return { summary, changedFiles: input.role === "implementation" ? ["src/change.ts"] : [] };
  }
}

class PausingAgentRunner implements AgentRunner {
  readonly roles: AgentRole[] = [];

  constructor(private readonly onStarted: () => void) {}

  async run(input: AgentRunInput): Promise<AgentRunResult> {
    this.roles.push(input.role);
    input.onEvent({ type: "started", message: `${input.role} started` });
    if (input.role === "triage") {
      this.onStarted();
      await new Promise<never>((_resolve, reject) => {
        if (input.signal.aborted) {
          reject(new DOMException("Aborted", "AbortError"));
          return;
        }
        input.signal.addEventListener("abort", () => reject(new DOMException("Aborted", "AbortError")), { once: true });
      });
    }
    return {
      summary: input.role === "triage"
        ? JSON.stringify({ route: "auto_resolvable", reason: "bounded task" })
        : `${input.role} completed`,
      changedFiles: input.role === "implementation" ? ["src/resumed.ts"] : [],
    };
  }
}

function queuedTask(db: Awaited<ReturnType<typeof openStorageDatabase>>, id: string, description: string): void {
  saveQueueItem(db, { id, description });
}

test("routes auto-resolvable work through verification to final human review and holds the queue", async () => {
  const db = await openStorageDatabase(":memory:");
  const provider = new FakeWorkspaceProvider();
  const runner = new FakeAgentRunner("auto_resolvable", "A bounded code change");
  const events: string[] = [];
  queuedTask(db, "task-a", "Add a small feature");
  queuedTask(db, "task-b", "Fix another issue");

  try {
    const dependencies = {
      db,
      workspaceProvider: provider,
      agentRunner: runner,
      repositoryPath: "/repo",
      baseBranch: "main",
      onAgentEvent: (_taskId: string, event: { type: string }) => events.push(event.type),
    };
    const first = await processNextTask(dependencies);
    expect(first.status).toBe("review_pending");
    if (first.status !== "review_pending") throw new Error("Expected final review checkpoint");
    expect(first.taskId).toBe("task-a");
    expect(first.classification.route).toBe("auto_resolvable");
    expect(first.review.verificationSummary).toBe("verification passed");
    expect(first.review.changedFiles).toEqual(["src/change.ts"]);

    expect(await processNextTask(dependencies)).toEqual({ status: "busy", taskId: "task-a" });
    expect(runner.roles).toEqual(["triage", "implementation", "code-review", "verification"]);
    expect(events).toHaveLength(8);
    expect(provider.calls).toEqual([
      "create:task-a",
      "open:triage",
      "close-pane:pane-triage",
      "open:implementation",
      "close-pane:pane-implementation",
      "open:code-review",
      "close-pane:pane-code-review",
      "open:verification",
      "close-pane:pane-verification",
    ]);
    expect(db.query<{ status: string }, [string]>("SELECT status FROM tasks WHERE id = ?").get("task-a")?.status)
      .toBe("review_pending");
    expect(db.query<{ status: string; current_stage: string }, [string]>(
      "SELECT status, current_stage FROM workflows WHERE task_id = ?",
    ).get("task-a")).toEqual({ status: "running", current_stage: "final_review" });
    expect(db.query<{ count: number }, [string]>(
      "SELECT COUNT(*) AS count FROM agent_run_events WHERE agent_run_id IN (SELECT id FROM agent_runs WHERE task_id = ?)",
    ).get("task-a")?.count).toBe(8);
    const agentRuns = db.query<{ role: string; status: string }, [string]>(
      "SELECT role, status FROM agent_runs WHERE task_id = ? ORDER BY role",
    ).all("task-a");
    expect(agentRuns).toEqual([
      { role: "code-review", status: "succeeded" },
      { role: "implementation", status: "succeeded" },
      { role: "triage", status: "succeeded" },
      { role: "verification", status: "succeeded" },
    ]);
    const implementationRun = db.query<{ result: string }, [string]>(
      "SELECT result FROM agent_runs WHERE task_id = ? AND role = 'implementation'",
    ).get("task-a");
    expect(JSON.parse(implementationRun!.result).summary).toBe("implementation completed");
    expect(db.query<{ status: string }, [string]>("SELECT status FROM tasks WHERE id = ?").get("task-b")?.status)
      .toBe("queued");
  } finally {
    db.close(true);
  }
});

test("blocks clarification and specification routes without starting implementation", async () => {
  for (const route of ["clarification_required", "specification_required"]) {
    const db = await openStorageDatabase(":memory:");
    const provider = new FakeWorkspaceProvider();
    const runner = new FakeAgentRunner(route, "The task needs human input");
    queuedTask(db, `task-${route}`, "A task outside the automatic path");

    try {
      const result = await processNextTask({
        db,
        workspaceProvider: provider,
        agentRunner: runner,
        repositoryPath: "/repo",
        baseBranch: "main",
      });
      expect(result.status).toBe("blocked");
      if (result.status !== "blocked") throw new Error("Expected task to be blocked");
      expect(result.classification).toEqual({ route, reason: "The task needs human input" });
      expect(runner.roles).toEqual(["triage"]);
      expect(provider.calls).toEqual([
        `create:task-${route}`,
        "open:triage",
        "close-pane:pane-triage",
        `close-workspace:workspace-task-${route}`,
      ]);
      expect(db.query<{ status: string }, [string]>(
        "SELECT status FROM tasks WHERE id = ?",
      ).get(`task-${route}`)?.status).toBe("blocked");
      expect(db.query<{ status: string; current_stage: string; last_error: string }, [string]>(
        "SELECT status, current_stage, last_error FROM workflows WHERE task_id = ?",
      ).get(`task-${route}`)).toEqual({
        status: "blocked",
        current_stage: "blocked",
        last_error: "The task needs human input",
      });
    } finally {
      db.close(true);
    }
  }
});

test("startup reconciliation interrupts active work and explicit resume restores its checkpoint", async () => {
  const db = await openStorageDatabase(":memory:");
  const provider = new FakeWorkspaceProvider();
  queuedTask(db, "task-restart", "Continue interrupted work");
  const controller = new AbortController();
  let markTriageStarted!: () => void;
  const triageStarted = new Promise<void>((resolve) => { markTriageStarted = resolve; });
  const firstRunner = new PausingAgentRunner(() => markTriageStarted());
  const initialDependencies = {
    db,
    workspaceProvider: provider,
    agentRunner: firstRunner,
    repositoryPath: "/repo",
    baseBranch: "main",
    signal: controller.signal,
  };

  try {
    const inFlight = processNextTask(initialDependencies);
    await triageStarted;
    const reconciliation = await reconcileActiveTasks(initialDependencies);
    expect(reconciliation).toHaveLength(1);
    expect(reconciliation[0]).toMatchObject({
      taskId: "task-restart",
      runtimeSuspended: true,
    });
    expect(firstRunner.roles).toEqual(["triage"]);
    expect(db.query<{ status: string }, [string]>("SELECT status FROM tasks WHERE id = ?")
      .get("task-restart")?.status).toBe("interrupted");
    expect(provider.calls).toContain("suspend:workspace-task-restart");

    controller.abort();
    expect(await inFlight).toMatchObject({ status: "resume_failed", taskId: "task-restart" });
    expect(firstRunner.roles).toEqual(["triage"]);

    provider.failRestore = true;
    const unavailable = await resumeInterruptedTask({ ...initialDependencies, signal: undefined }, "task-restart");
    expect(unavailable).toMatchObject({ status: "resume_failed", taskId: "task-restart" });
    expect(db.query<{ status: string }, [string]>("SELECT status FROM tasks WHERE id = ?")
      .get("task-restart")?.status).toBe("interrupted");
    expect(firstRunner.roles).toEqual(["triage"]);

    provider.failRestore = false;
    const resumeRunner = new FakeAgentRunner("auto_resolvable", "Resumed from checkpoint");
    const resumed = await resumeInterruptedTask({
      db,
      workspaceProvider: provider,
      agentRunner: resumeRunner,
      repositoryPath: "/repo",
      baseBranch: "main",
    }, "task-restart");
    expect(resumed.status).toBe("review_pending");
    expect(resumeRunner.roles).toEqual(["triage", "implementation", "code-review", "verification"]);
    expect(provider.calls).toContain("restore:workspace-task-restart");
    expect(db.query<{ status: string }, [string]>("SELECT status FROM tasks WHERE id = ?")
      .get("task-restart")?.status).toBe("review_pending");
  } finally {
    controller.abort();
    db.close(true);
  }
});

test("keeps a task interrupted when its checkpoint is unavailable", async () => {
  const db = await openStorageDatabase(":memory:");
  const provider = new FakeWorkspaceProvider();
  queuedTask(db, "task-missing-checkpoint", "Resume requires a checkpoint");
  try {
    const claim = claimNextTask(db);
    if (claim.status !== "claimed") throw new Error("Expected a task claim");
    const workspace = {
      id: "workspace-task-missing-checkpoint",
      rootPath: "/tmp/task-missing-checkpoint",
    };
    setWorkflowWorkspace(db, claim.task.id, workspace);
    markTaskInterrupted(db, claim.task.id, "Cronos restarted");

    const result = await resumeInterruptedTask({
      db,
      workspaceProvider: provider,
      agentRunner: new FakeAgentRunner("auto_resolvable", "No run without checkpoint"),
      repositoryPath: "/repo",
      baseBranch: "main",
    }, claim.task.id);
    expect(result).toMatchObject({
      status: "resume_failed",
      reason: "Workflow checkpoint or persisted workspace is unavailable",
    });
    expect(db.query<{ status: string }, [string]>("SELECT status FROM tasks WHERE id = ?")
      .get(claim.task.id)?.status).toBe("interrupted");
    expect(provider.calls).toEqual([]);
  } finally {
    db.close(true);
  }
});

test("approval documents, creates the pull request, and records the task delivered", async () => {
  const db = await openStorageDatabase(":memory:");
  const provider = new FakeWorkspaceProvider();
  const runner = new FakeAgentRunner("auto_resolvable", "A bounded code change");
  queuedTask(db, "task-approve", "Add a small feature");
  const dependencies = {
    db,
    workspaceProvider: provider,
    agentRunner: runner,
    repositoryPath: "/repo",
    baseBranch: "main",
    pullRequestCreator: {
      async createPullRequest(input) {
        expect(input.taskId).toBe("task-approve");
        expect(input.documentationSummary).toBe("documentation completed");
        expect(input.verificationSummary).toBe("verification passed");
        return {
          branch: "cronos/task-approve",
          commitSha: "a".repeat(40),
          number: 42,
          url: "https://github.com/example/repo/pull/42",
          title: "Cronos: Add a small feature",
        };
      },
    },
  };

  try {
    const pending = await processNextTask(dependencies);
    expect(pending.status).toBe("review_pending");
    const delivered = await resolveTaskReview(dependencies, "task-approve", { approved: true, feedback: "" });
    expect(delivered.status).toBe("delivered");
    if (delivered.status !== "delivered") throw new Error("Expected delivery to create a pull request");
    expect(delivered.documentationSummary).toBe("documentation completed");
    expect(delivered.pullRequest).toMatchObject({ number: 42, url: "https://github.com/example/repo/pull/42" });
    expect(delivered.changedFiles).toContain("src/change.ts");
    expect(runner.roles).toEqual(["triage", "implementation", "code-review", "verification", "documentation"]);
    expect(db.query<{ status: string }, [string]>("SELECT status FROM tasks WHERE id = ?").get("task-approve")?.status)
      .toBe("delivered");
    expect(db.query<{ current_stage: string; pull_request_number: number; pull_request_url: string }, [string]>(
      "SELECT current_stage, pull_request_number, pull_request_url FROM workflows WHERE task_id = ?",
    ).get("task-approve")).toEqual({
      current_stage: "delivered",
      pull_request_number: 42,
      pull_request_url: "https://github.com/example/repo/pull/42",
    });
    expect(provider.calls).toContain("close-workspace:workspace-task-approve");
    expect(await resolveTaskReview(dependencies, "task-approve", { approved: true, feedback: "" }))
      .toMatchObject({ status: "errored", reason: "Task is not awaiting final review" });
    expect(runner.roles).toHaveLength(5);
  } finally {
    db.close(true);
  }
});

test("rejection returns to implementation and repeats review and verification before asking again", async () => {
  const db = await openStorageDatabase(":memory:");
  const provider = new FakeWorkspaceProvider();
  const runner = new FakeAgentRunner("auto_resolvable", "A bounded code change");
  queuedTask(db, "task-rework", "Add a small feature");
  const dependencies = {
    db,
    workspaceProvider: provider,
    agentRunner: runner,
    repositoryPath: "/repo",
    baseBranch: "main",
  };

  try {
    const firstReview = await processNextTask(dependencies);
    expect(firstReview.status).toBe("review_pending");
    const repeatedReview = await resolveTaskReview(dependencies, "task-rework", {
      approved: false,
      feedback: "Add an edge-case test",
    });
    expect(repeatedReview.status).toBe("review_pending");
    expect(runner.roles).toEqual([
      "triage", "implementation", "code-review", "verification",
      "implementation", "code-review", "verification",
    ]);
    expect(runner.prompts.filter((prompt) => prompt.startsWith("Implement the requested task"))[1])
      .toContain("Add an edge-case test");
    const ready = await resolveTaskReview(dependencies, "task-rework", { approved: true, feedback: "" });
    expect(ready.status).toBe("ready_for_delivery");
    expect(runner.roles.at(-1)).toBe("documentation");
  } finally {
    db.close(true);
  }
});

test("quality-gate failures are persisted and stop before final human review", async () => {
  for (const gate of ["codeReview", "verification"] as const) {
    const db = await openStorageDatabase(":memory:");
    const provider = new FakeWorkspaceProvider();
    const runner = new FakeAgentRunner("auto_resolvable", "A bounded code change", { [gate]: "failed" });
    const taskId = `task-${gate}-failed`;
    queuedTask(db, taskId, "Add a small feature");

    try {
      const result = await processNextTask({
        db,
        workspaceProvider: provider,
        agentRunner: runner,
        repositoryPath: "/repo",
        baseBranch: "main",
      });
      expect(result.status).toBe("errored");
      expect(runner.roles).toEqual(gate === "codeReview"
        ? ["triage", "implementation", "code-review"]
        : ["triage", "implementation", "code-review", "verification"]);
      expect(runner.roles).not.toContain("documentation");
      expect(db.query<{ status: string; current_stage: string }, [string]>(
        "SELECT status, current_stage FROM workflows WHERE task_id = ?",
      ).get(taskId)).toEqual({ status: "errored", current_stage: "errored" });
    } finally {
      db.close(true);
    }
  }
});

test("fails closed when triage output does not match the supported route schema", async () => {
  const db = await openStorageDatabase(":memory:");
  const provider = new FakeWorkspaceProvider();
  const runner = new FakeAgentRunner("implement_now", "Use an unsupported route");
  queuedTask(db, "task-invalid-route", "Do not implement before classification");

  try {
    const result = await processNextTask({
      db,
      workspaceProvider: provider,
      agentRunner: runner,
      repositoryPath: "/repo",
      baseBranch: "main",
    });
    expect(result).toMatchObject({ status: "errored", taskId: "task-invalid-route", reason: "Task workflow failed" });
    expect(runner.roles).toEqual(["triage"]);
    expect(provider.calls).toContain("close-workspace:workspace-task-invalid-route");
    expect(db.query<{ status: string }, [string]>(
      "SELECT status FROM tasks WHERE id = ?",
    ).get("task-invalid-route")?.status).toBe("errored");
    expect(db.query<{ role: string; status: string }, [string]>(
      "SELECT role, status FROM agent_runs WHERE task_id = ?",
    ).get("task-invalid-route")).toEqual({ role: "triage", status: "failed" });
  } finally {
    db.close(true);
  }
});
