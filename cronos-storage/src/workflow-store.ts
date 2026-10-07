import type { Database } from "bun:sqlite";

const MAX_AGENT_RESULT_CHARS = 256_000;
const MAX_AGENT_EVENT_MESSAGE_CHARS = 4_000;

export type ClaimedTask = {
  id: string;
  description: string;
  status: "active";
  createdAt: string;
  updatedAt: string;
};

export type TaskStatus = "queued" | "active" | "blocked" | "errored" | "interrupted" | "review_pending" | "delivered";

export type TaskClaim =
  | { status: "claimed"; task: ClaimedTask }
  | { status: "busy"; taskId: string }
  | { status: "idle" };

export type ActiveWorkflowTask = {
  id: string;
  description: string;
  currentStage: string;
  workspace: { id: string; taskId: string; rootPath: string } | null;
};

export type PullRequestDelivery = {
  number: number;
  url: string;
  branch: string;
};

type TaskRow = {
  id: string;
  description: string;
  created_at: string;
  updated_at: string;
};

function toClaimedTask(row: TaskRow, updatedAt: string): ClaimedTask {
  return {
    id: row.id,
    description: row.description,
    status: "active",
    createdAt: row.created_at,
    updatedAt,
  };
}

export function claimNextTask(db: Database): TaskClaim {
  const claim = db.transaction((): TaskClaim => {
    const active = db.query<{ id: string }, []>(`
      SELECT id FROM tasks WHERE status IN ('active', 'review_pending', 'interrupted') LIMIT 1
    `).get();
    if (active) return { status: "busy", taskId: active.id };

    const row = db.query<TaskRow, []>(`
      SELECT id, description, created_at, updated_at
      FROM tasks
      WHERE status = 'queued'
      ORDER BY created_at ASC, id ASC
      LIMIT 1
    `).get();
    if (!row) return { status: "idle" };

    const now = new Date().toISOString();
    const update = db.query(`
      UPDATE tasks SET status = 'active', updated_at = ?
      WHERE id = ? AND status = 'queued'
    `).run(now, row.id);
    if (update.changes !== 1) return { status: "idle" };

    db.query(`
      INSERT INTO workflows (task_id, status, current_stage, last_error, created_at, updated_at)
      VALUES (?, 'running', 'triage', NULL, ?, ?)
    `).run(row.id, now, now);

    return { status: "claimed", task: toClaimedTask(row, now) };
  });

  try {
    return claim();
  } catch (error) {
    // The partial unique index is the cross-connection guard if another worker claimed first.
    if (error instanceof Error && error.message.includes("UNIQUE constraint failed")) {
      const active = db.query<{ id: string }, []>(`
        SELECT id FROM tasks WHERE status IN ('active', 'review_pending', 'interrupted') LIMIT 1
      `).get();
      if (active) return { status: "busy", taskId: active.id };
    }
    throw error;
  }
}

export function getTaskStatus(db: Database, taskId: string): TaskStatus | null {
  return db.query<{ status: TaskStatus }, [string]>(
    "SELECT status FROM tasks WHERE id = ?",
  ).get(taskId)?.status ?? null;
}

export function beginTaskReviewResolution(db: Database, taskId: string): boolean {
  const now = new Date().toISOString();
  const transaction = db.transaction(() => {
    const taskResult = db.query(`
      UPDATE tasks SET status = 'active', updated_at = ?
      WHERE id = ? AND status = 'review_pending'
    `).run(now, taskId);
    if (taskResult.changes !== 1) return false;

    const workflowResult = db.query(`
      UPDATE workflows SET status = 'running', current_stage = 'final_review', updated_at = ?
      WHERE task_id = ?
    `).run(now, taskId);
    if (workflowResult.changes !== 1) throw new Error(`Workflow record is missing for task ${taskId}`);
    return true;
  });
  return transaction();
}

export function setWorkflowWorkspace(
  db: Database,
  taskId: string,
  workspace: { id: string; rootPath: string },
): void {
  const result = db.query(`
    UPDATE workflows SET workspace_id = ?, workspace_path = ?, updated_at = ?
    WHERE task_id = ? AND status = 'running'
  `).run(workspace.id, workspace.rootPath, new Date().toISOString(), taskId);
  if (result.changes !== 1) throw new Error(`Workflow record is missing for task ${taskId}`);
}

export function getWorkflowWorkspace(
  db: Database,
  taskId: string,
): ActiveWorkflowTask["workspace"] {
  const row = db.query<{ workspace_id: string | null; workspace_path: string | null }, [string]>(`
    SELECT workspace_id, workspace_path FROM workflows WHERE task_id = ?
  `).get(taskId);
  if (!row?.workspace_id || !row.workspace_path) return null;
  return { id: row.workspace_id, taskId, rootPath: row.workspace_path };
}

export function listActiveWorkflowTasks(db: Database): ActiveWorkflowTask[] {
  const rows = db.query<{
    id: string;
    description: string;
    current_stage: string;
    workspace_id: string | null;
    workspace_path: string | null;
  }, []>(`
    SELECT t.id, t.description, w.current_stage, w.workspace_id, w.workspace_path
    FROM tasks t JOIN workflows w ON w.task_id = t.id
    WHERE t.status = 'active'
    ORDER BY t.updated_at ASC, t.id ASC
  `).all();
  return rows.map((row) => ({
    id: row.id,
    description: row.description,
    currentStage: row.current_stage,
    workspace: row.workspace_id && row.workspace_path
      ? { id: row.workspace_id, taskId: row.id, rootPath: row.workspace_path }
      : null,
  }));
}

export function markTaskInterrupted(db: Database, taskId: string, reason: string): void {
  const now = new Date().toISOString();
  const transaction = db.transaction(() => {
    const taskResult = db.query(`
      UPDATE tasks SET status = 'interrupted', updated_at = ? WHERE id = ? AND status = 'active'
    `).run(now, taskId);
    const workflowResult = db.query(`
      UPDATE workflows SET status = 'interrupted', last_error = ?, updated_at = ? WHERE task_id = ?
    `).run(reason.slice(0, 4_000), now, taskId);
    if (taskResult.changes !== 1 || workflowResult.changes !== 1) {
      throw new Error(`Cannot interrupt task ${taskId} without an active workflow`);
    }
  });
  transaction();
}

export function resumeInterruptedTask(db: Database, taskId: string): boolean {
  const now = new Date().toISOString();
  const transaction = db.transaction(() => {
    const taskResult = db.query(`
      UPDATE tasks SET status = 'active', updated_at = ? WHERE id = ? AND status = 'interrupted'
    `).run(now, taskId);
    if (taskResult.changes !== 1) return false;
    const workflowResult = db.query(`
      UPDATE workflows SET status = 'running', last_error = NULL, updated_at = ? WHERE task_id = ?
    `).run(now, taskId);
    if (workflowResult.changes !== 1) throw new Error(`Workflow record is missing for task ${taskId}`);
    return true;
  });
  return transaction();
}

export function setTaskReviewPending(db: Database, taskId: string): void {
  const now = new Date().toISOString();
  const transaction = db.transaction(() => {
    const taskResult = db.query(`
      UPDATE tasks SET status = 'review_pending', updated_at = ?
      WHERE id = ? AND status IN ('active', 'review_pending')
    `).run(now, taskId);
    const workflowResult = db.query(`
      UPDATE workflows SET status = 'running', current_stage = 'final_review', updated_at = ?
      WHERE task_id = ?
    `).run(now, taskId);
    if (taskResult.changes !== 1 || workflowResult.changes !== 1) {
      throw new Error(`Cannot request review for task ${taskId} without an active workflow`);
    }
  });
  transaction();
}

export function setTaskActive(db: Database, taskId: string, stage: string): void {
  const now = new Date().toISOString();
  const transaction = db.transaction(() => {
    const taskResult = db.query(`
      UPDATE tasks SET status = 'active', updated_at = ?
      WHERE id = ? AND status IN ('active', 'review_pending')
    `).run(now, taskId);
    const workflowResult = db.query(`
      UPDATE workflows SET status = 'running', current_stage = ?, updated_at = ?
      WHERE task_id = ?
    `).run(stage, now, taskId);
    if (taskResult.changes !== 1 || workflowResult.changes !== 1) {
      throw new Error(`Cannot resume task ${taskId} without a running workflow`);
    }
  });
  transaction();
}

export function setWorkflowStage(db: Database, taskId: string, stage: string): void {
  const now = new Date().toISOString();
  const result = db.query(`
    UPDATE workflows SET status = 'running', current_stage = ?, updated_at = ?
    WHERE task_id = ?
  `).run(stage, now, taskId);
  if (result.changes !== 1) throw new Error(`Workflow record is missing for task ${taskId}`);
}

export function blockTask(db: Database, taskId: string, reason: string): void {
  const now = new Date().toISOString();
  const updateTask = db.query(`
    UPDATE tasks SET status = 'blocked', updated_at = ? WHERE id = ? AND status = 'active'
  `);
  const updateWorkflow = db.query(`
    UPDATE workflows
    SET status = 'blocked', current_stage = 'blocked', last_error = ?, updated_at = ?
    WHERE task_id = ?
  `);
  const transaction = db.transaction(() => {
    const taskResult = updateTask.run(now, taskId);
    const workflowResult = updateWorkflow.run(reason.slice(0, 2_000), now, taskId);
    if (taskResult.changes !== 1 || workflowResult.changes !== 1) {
      throw new Error(`Cannot block task ${taskId} without an active workflow`);
    }
  });
  transaction();
}

export function failTask(db: Database, taskId: string, reason: string): void {
  const now = new Date().toISOString();
  const updateTask = db.query(`
    UPDATE tasks SET status = 'errored', updated_at = ? WHERE id = ? AND status = 'active'
  `);
  const updateWorkflow = db.query(`
    UPDATE workflows
    SET status = 'errored', current_stage = 'errored', last_error = ?, updated_at = ?
    WHERE task_id = ?
  `);
  const transaction = db.transaction(() => {
    const taskResult = updateTask.run(now, taskId);
    const workflowResult = updateWorkflow.run(reason.slice(0, 4_000), now, taskId);
    if (taskResult.changes !== 1 || workflowResult.changes !== 1) {
      throw new Error(`Cannot mark task ${taskId} errored without an active workflow`);
    }
  });
  transaction();
}

export function markTaskDelivered(db: Database, taskId: string, pullRequest: PullRequestDelivery): void {
  if (!Number.isSafeInteger(pullRequest.number) || pullRequest.number <= 0) {
    throw new Error("Pull request number must be a positive safe integer");
  }
  const pullRequestUrl = /^https:\/\/github\.com\/[A-Za-z0-9-]+\/[A-Za-z0-9_.-]+\/pull\/([1-9]\d*)$/.exec(pullRequest.url);
  if (!pullRequestUrl || Number(pullRequestUrl[1]) !== pullRequest.number) {
    throw new Error("Pull request URL must match the GitHub pull request number");
  }
  if (!pullRequest.branch || pullRequest.branch.length > 255 || /[\u0000-\u001f\u007f]/.test(pullRequest.branch)) {
    throw new Error("Pull request branch is invalid");
  }

  const now = new Date().toISOString();
  const transaction = db.transaction(() => {
    const taskResult = db.query(`
      UPDATE tasks SET status = 'delivered', updated_at = ? WHERE id = ? AND status = 'active'
    `).run(now, taskId);
    const workflowResult = db.query(`
      UPDATE workflows
      SET status = 'completed', current_stage = 'delivered', last_error = NULL,
          pull_request_number = ?, pull_request_url = ?, pull_request_branch = ?,
          workspace_id = NULL, workspace_path = NULL, updated_at = ?
      WHERE task_id = ? AND status = 'running' AND current_stage = 'pull_request'
        AND pull_request_number IS NULL AND pull_request_url IS NULL AND pull_request_branch IS NULL
    `).run(pullRequest.number, pullRequest.url, pullRequest.branch, now, taskId);
    if (taskResult.changes !== 1 || workflowResult.changes !== 1) {
      throw new Error(`Cannot mark task ${taskId} delivered before pull-request creation`);
    }
  });
  transaction();
}

export function startAgentRun(db: Database, taskId: string, role: string): string {
  const id = crypto.randomUUID();
  db.query(`
    INSERT INTO agent_runs (id, task_id, role, status, created_at, started_at)
    VALUES (?, ?, ?, 'running', ?, ?)
  `).run(id, taskId, role, new Date().toISOString(), new Date().toISOString());
  return id;
}

export function recordAgentRunEvent(
  db: Database,
  agentRunId: string,
  event: { type: "started" | "progress" | "completed" | "failed"; message: string },
): void {
  db.query(`
    INSERT INTO agent_run_events (agent_run_id, type, message, created_at)
    VALUES (?, ?, ?, ?)
  `).run(agentRunId, event.type, event.message.slice(0, MAX_AGENT_EVENT_MESSAGE_CHARS), new Date().toISOString());
}

export function completeAgentRun(db: Database, id: string, result: unknown): void {
  const serialized = JSON.stringify(result);
  if (serialized === undefined || serialized.length > MAX_AGENT_RESULT_CHARS) {
    throw new Error("Agent result is not serializable or exceeds the storage limit");
  }
  const response = db.query(`
    UPDATE agent_runs SET status = 'succeeded', result = ?, finished_at = ?
    WHERE id = ? AND status = 'running'
  `).run(serialized, new Date().toISOString(), id);
  if (response.changes !== 1) throw new Error(`Agent run ${id} is not active`);
}

export function failAgentRun(db: Database, id: string, reason: string): void {
  const response = db.query(`
    UPDATE agent_runs SET status = 'failed', error = ?, finished_at = ?
    WHERE id = ? AND status = 'running'
  `).run(reason.slice(0, MAX_AGENT_EVENT_MESSAGE_CHARS), new Date().toISOString(), id);
  if (response.changes !== 1) throw new Error(`Agent run ${id} is not active`);
}
