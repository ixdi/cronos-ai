import { expect, test } from "bun:test";
import { openStorageDatabase } from "./database";
import {
  beginTaskReviewResolution,
  blockTask,
  claimNextTask,
  completeAgentRun,
  failAgentRun,
  getTaskStatus,
  getWorkflowWorkspace,
  listActiveWorkflowTasks,
  markTaskInterrupted,
  markTaskDelivered,
  setWorkflowStage,
  failTask,
  recordAgentRunEvent,
  setTaskActive,
  setTaskReviewPending,
  setWorkflowWorkspace,
  resumeInterruptedTask,
  startAgentRun,
} from "./workflow-store";

function insertQueuedTask(db: Awaited<ReturnType<typeof openStorageDatabase>>, id: string, createdAt: string): void {
  db.query(`
    INSERT INTO tasks (id, description, status, created_at, updated_at)
    VALUES (?, ?, 'queued', ?, ?)
  `).run(id, `Task ${id}`, createdAt, createdAt);
}

test("claims queued tasks in order and refuses a second active task", async () => {
  const db = await openStorageDatabase(":memory:");
  const createdAt = "2026-01-01T00:00:00.000Z";
  insertQueuedTask(db, "task-b", createdAt);
  insertQueuedTask(db, "task-a", createdAt);

  try {
    const first = claimNextTask(db);
    expect(first.status).toBe("claimed");
    if (first.status !== "claimed") throw new Error("Expected a task claim");
    expect(first.task.id).toBe("task-a");
    expect(claimNextTask(db)).toEqual({ status: "busy", taskId: "task-a" });
    expect(db.query<{ status: string }, []>("SELECT status FROM tasks WHERE status = 'active'").all()).toHaveLength(1);
    expect(db.query<{ current_stage: string }, [string]>(
      "SELECT current_stage FROM workflows WHERE task_id = ?",
    ).get("task-a")?.current_stage).toBe("triage");
    expect(db.query<{ status: string }, [string]>(
      "SELECT status FROM tasks WHERE id = ?",
    ).get("task-b")?.status).toBe("queued");

    setTaskReviewPending(db, "task-a");
    expect(claimNextTask(db)).toEqual({ status: "busy", taskId: "task-a" });
    expect(beginTaskReviewResolution(db, "task-a")).toBe(true);
    expect(beginTaskReviewResolution(db, "task-a")).toBe(false);
    setTaskActive(db, "task-a", "implementation");
    expect(claimNextTask(db)).toEqual({ status: "busy", taskId: "task-a" });
  } finally {
    db.close(true);
  }
});

test("persists workspace handles and requires explicit resume after interruption", async () => {
  const db = await openStorageDatabase(":memory:");
  insertQueuedTask(db, "task-resume", "2026-01-01T00:00:00.000Z");
  try {
    const claim = claimNextTask(db);
    if (claim.status !== "claimed") throw new Error("Expected a task claim");
    const workspace = { id: "workspace-resume", taskId: claim.task.id, rootPath: "/runtime/worktree-resume" };
    setWorkflowWorkspace(db, claim.task.id, workspace);
    expect(getWorkflowWorkspace(db, claim.task.id)).toEqual(workspace);
    expect(listActiveWorkflowTasks(db)).toEqual([{
      id: claim.task.id,
      description: claim.task.description,
      currentStage: "triage",
      workspace,
    }]);

    markTaskInterrupted(db, claim.task.id, "Cronos restarted");
    expect(claimNextTask(db)).toEqual({ status: "busy", taskId: claim.task.id });
    expect(listActiveWorkflowTasks(db)).toEqual([]);
    expect(resumeInterruptedTask(db, claim.task.id)).toBe(true);
    expect(resumeInterruptedTask(db, claim.task.id)).toBe(false);
    expect(claimNextTask(db)).toEqual({ status: "busy", taskId: claim.task.id });
  } finally {
    db.close(true);
  }
});

test("marks tasks delivered atomically with a validated pull-request reference", async () => {
  const db = await openStorageDatabase(":memory:");
  insertQueuedTask(db, "task-delivered", "2026-01-01T00:00:00.000Z");
  insertQueuedTask(db, "task-next", "2026-01-02T00:00:00.000Z");
  try {
    const claim = claimNextTask(db);
    if (claim.status !== "claimed") throw new Error("Expected a task claim");
    setWorkflowWorkspace(db, claim.task.id, { id: "workspace-delivered", rootPath: "/runtime/worktree-delivered" });
    setWorkflowStage(db, claim.task.id, "pull_request");
    markTaskDelivered(db, claim.task.id, {
      number: 42,
      url: "https://github.com/example/repo/pull/42",
      branch: "cronos/task-delivered",
    });

    expect(getTaskStatus(db, claim.task.id)).toBe("delivered");
    expect(db.query<{
      status: string;
      current_stage: string;
      pull_request_number: number;
      pull_request_url: string;
      pull_request_branch: string;
      workspace_id: string | null;
    }, [string]>(`
      SELECT status, current_stage, pull_request_number, pull_request_url, pull_request_branch, workspace_id
      FROM workflows WHERE task_id = ?
    `).get(claim.task.id)).toEqual({
      status: "completed",
      current_stage: "delivered",
      pull_request_number: 42,
      pull_request_url: "https://github.com/example/repo/pull/42",
      pull_request_branch: "cronos/task-delivered",
      workspace_id: null,
    });
    expect(claimNextTask(db)).toMatchObject({ status: "claimed", task: { id: "task-next" } });
    expect(() => markTaskDelivered(db, claim.task.id, {
      number: 42,
      url: "https://github.com/example/repo/pull/42",
      branch: "cronos/task-delivered",
    })).toThrow("Cannot mark task");
    expect(db.query<{ pull_request_number: number; pull_request_url: string }, [string]>(
      "SELECT pull_request_number, pull_request_url FROM workflows WHERE task_id = ?",
    ).get("task-delivered")).toEqual({
      pull_request_number: 42,
      pull_request_url: "https://github.com/example/repo/pull/42",
    });
    expect(() => markTaskDelivered(db, "task-missing", {
      number: 9,
      url: "https://github.com/example/repo/pull/10",
      branch: "branch",
    })).toThrow("match the GitHub pull request number");
  } finally {
    db.close(true);
  }
});

test("bounds agent event and result payloads before storing them", async () => {
  const db = await openStorageDatabase(":memory:");
  insertQueuedTask(db, "task-bounds", "2026-01-01T00:00:00.000Z");
  try {
    const claim = claimNextTask(db);
    if (claim.status !== "claimed") throw new Error("Expected a task claim");
    const agentRunId = startAgentRun(db, claim.task.id, "triage");
    recordAgentRunEvent(db, agentRunId, { type: "progress", message: "x".repeat(5_000) });
    expect(db.query<{ length: number }, [string]>(
      "SELECT length(message) AS length FROM agent_run_events WHERE agent_run_id = ?",
    ).get(agentRunId)?.length).toBe(4_000);
    expect(() => completeAgentRun(db, agentRunId, { summary: "x".repeat(256_001) }))
      .toThrow("exceeds the storage limit");
  } finally {
    db.close(true);
  }
});

test("persists blocked and errored task outcomes and agent lifecycle", async () => {
  const db = await openStorageDatabase(":memory:");
  insertQueuedTask(db, "task-blocked", "2026-01-01T00:00:00.000Z");

  try {
    const claim = claimNextTask(db);
    if (claim.status !== "claimed") throw new Error("Expected a task claim");
    const agentRunId = startAgentRun(db, claim.task.id, "triage");
    recordAgentRunEvent(db, agentRunId, { type: "started", message: "Triage started" });
    recordAgentRunEvent(db, agentRunId, { type: "progress", message: "Classifier returned" });
    completeAgentRun(db, agentRunId, { summary: "Needs clarification" });
    blockTask(db, claim.task.id, "Human clarification is required");

    expect(db.query<{ status: string }, [string]>(
      "SELECT status FROM tasks WHERE id = ?",
    ).get(claim.task.id)?.status).toBe("blocked");
    expect(db.query<{ status: string; current_stage: string; last_error: string }, [string]>(
      "SELECT status, current_stage, last_error FROM workflows WHERE task_id = ?",
    ).get(claim.task.id)).toMatchObject({
      status: "blocked",
      current_stage: "blocked",
      last_error: "Human clarification is required",
    });
    expect(db.query<{ status: string; result: string }, [string]>(
      "SELECT status, result FROM agent_runs WHERE id = ?",
    ).get(agentRunId)).toMatchObject({ status: "succeeded", result: '{"summary":"Needs clarification"}' });
    expect(db.query<{ type: string; message: string }, [string]>(
      "SELECT type, message FROM agent_run_events WHERE agent_run_id = ? ORDER BY id",
    ).all(agentRunId)).toEqual([
      { type: "started", message: "Triage started" },
      { type: "progress", message: "Classifier returned" },
    ]);

    insertQueuedTask(db, "task-error", "2026-01-01T00:00:01.000Z");
    const next = claimNextTask(db);
    if (next.status !== "claimed") throw new Error("Expected the next task to be claimed");
    const failedRunId = startAgentRun(db, next.task.id, "triage");
    failAgentRun(db, failedRunId, "Triage agent failed");
    failTask(db, next.task.id, "Triage agent failed");
    expect(db.query<{ status: string }, [string]>(
      "SELECT status FROM tasks WHERE id = ?",
    ).get(next.task.id)?.status).toBe("errored");
  } finally {
    db.close(true);
  }
});
