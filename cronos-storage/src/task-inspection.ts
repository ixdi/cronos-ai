import type { Database } from "bun:sqlite";
import type { TaskStatus } from "./workflow-store";

export type TaskSummary = {
  id: string;
  description: string;
  status: TaskStatus;
  createdAt: string;
  updatedAt: string;
  workflow: {
    status: string;
    currentStage: string;
    lastError: string | null;
    updatedAt: string;
    pullRequest: { number: number; url: string; branch: string } | null;
  } | null;
};

export type AgentRunInspection = {
  id: string;
  role: string;
  status: string;
  summary: string | null;
  structuredResult: unknown;
  changedFiles: string[];
  error: string | null;
  startedAt: string | null;
  finishedAt: string | null;
};

export type TaskInspection = TaskSummary & {
  agentRuns: AgentRunInspection[];
  codeReview: AgentRunInspection | null;
  verification: AgentRunInspection | null;
};

type TaskRow = {
  id: string;
  description: string;
  status: TaskStatus;
  created_at: string;
  updated_at: string;
  workflow_status: string | null;
  current_stage: string | null;
  last_error: string | null;
  workflow_updated_at: string | null;
  pull_request_number: number | null;
  pull_request_url: string | null;
  pull_request_branch: string | null;
};

type AgentRunRow = {
  id: string;
  role: string;
  status: string;
  result: string | null;
  error: string | null;
  started_at: string | null;
  finished_at: string | null;
};

function toTaskSummary(row: TaskRow): TaskSummary {
  return {
    id: row.id,
    description: row.description,
    status: row.status,
    createdAt: row.created_at,
    updatedAt: row.updated_at,
    workflow: row.workflow_status === null ? null : {
      status: row.workflow_status,
      currentStage: row.current_stage ?? "unknown",
      lastError: row.last_error,
      updatedAt: row.workflow_updated_at ?? row.updated_at,
      pullRequest: row.pull_request_number !== null && row.pull_request_url && row.pull_request_branch
        ? { number: row.pull_request_number, url: row.pull_request_url, branch: row.pull_request_branch }
        : null,
    },
  };
}

function toAgentRun(row: AgentRunRow): AgentRunInspection {
  let value: unknown = null;
  if (row.result !== null) {
    try {
      value = JSON.parse(row.result);
    } catch {
      value = { raw: row.result };
    }
  }
  const record = typeof value === "object" && value !== null && !Array.isArray(value)
    ? value as Record<string, unknown>
    : {};
  return {
    id: row.id,
    role: row.role,
    status: row.status,
    summary: typeof record.summary === "string" ? record.summary : null,
    structuredResult: record.structuredResult ?? null,
    changedFiles: Array.isArray(record.changedFiles)
      ? record.changedFiles.filter((file): file is string => typeof file === "string")
      : [],
    error: row.error,
    startedAt: row.started_at,
    finishedAt: row.finished_at,
  };
}

const TASK_SELECT = `
  SELECT t.id, t.description, t.status, t.created_at, t.updated_at,
         w.status AS workflow_status, w.current_stage, w.last_error,
         w.updated_at AS workflow_updated_at, w.pull_request_number,
         w.pull_request_url, w.pull_request_branch
  FROM tasks t LEFT JOIN workflows w ON w.task_id = t.id
`;

export function listTaskSummaries(db: Database, status?: TaskStatus): TaskSummary[] {
  const query = status
    ? `${TASK_SELECT} WHERE t.status = ? ORDER BY t.created_at ASC, t.id ASC`
    : `${TASK_SELECT} ORDER BY t.created_at ASC, t.id ASC`;
  const rows = status
    ? db.query<TaskRow, [TaskStatus]>(query).all(status)
    : db.query<TaskRow, []>(query).all();
  return rows.map(toTaskSummary);
}

export function inspectTask(db: Database, taskId: string): TaskInspection | null {
  const row = db.query<TaskRow, [string]>(`${TASK_SELECT} WHERE t.id = ?`).get(taskId);
  if (!row) return null;

  const agentRuns = db.query<AgentRunRow, [string]>(`
    SELECT id, role, status, result, error, started_at, finished_at
    FROM agent_runs WHERE task_id = ?
    ORDER BY created_at ASC, rowid ASC
  `).all(taskId).map(toAgentRun);

  return {
    ...toTaskSummary(row),
    agentRuns,
    codeReview: [...agentRuns].reverse().find((run) => run.role === "code-review") ?? null,
    verification: [...agentRuns].reverse().find((run) => run.role === "verification") ?? null,
  };
}
