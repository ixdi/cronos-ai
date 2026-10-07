CREATE TABLE tasks (
  id TEXT PRIMARY KEY,
  description TEXT NOT NULL CHECK (length(trim(description)) > 0),
  status TEXT NOT NULL CHECK (
    status IN ('queued', 'active', 'blocked', 'errored', 'interrupted', 'review_pending', 'delivered')
  ),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE INDEX tasks_status_created_idx ON tasks(status, created_at);

CREATE TABLE workflows (
  task_id TEXT PRIMARY KEY REFERENCES tasks(id) ON DELETE CASCADE,
  status TEXT NOT NULL CHECK (
    status IN ('pending', 'running', 'blocked', 'errored', 'interrupted', 'completed')
  ),
  current_stage TEXT NOT NULL,
  last_error TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE agent_runs (
  id TEXT PRIMARY KEY,
  task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
  role TEXT NOT NULL,
  status TEXT NOT NULL CHECK (
    status IN ('pending', 'running', 'succeeded', 'failed', 'interrupted')
  ),
  result TEXT,
  error TEXT,
  started_at TEXT,
  finished_at TEXT,
  created_at TEXT NOT NULL
);

CREATE INDEX agent_runs_task_created_idx ON agent_runs(task_id, created_at);

CREATE TABLE workflow_checkpoints (
  thread_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
  checkpoint_ns TEXT NOT NULL DEFAULT '',
  checkpoint_id TEXT NOT NULL,
  parent_checkpoint_id TEXT,
  checkpoint_type TEXT NOT NULL,
  checkpoint BLOB NOT NULL,
  metadata_type TEXT NOT NULL,
  metadata BLOB NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY (thread_id, checkpoint_ns, checkpoint_id)
);

CREATE TABLE workflow_checkpoint_writes (
  thread_id TEXT NOT NULL,
  checkpoint_ns TEXT NOT NULL DEFAULT '',
  checkpoint_id TEXT NOT NULL,
  writer_task_id TEXT NOT NULL,
  write_index INTEGER NOT NULL,
  channel TEXT NOT NULL,
  value_type TEXT NOT NULL,
  value BLOB NOT NULL,
  PRIMARY KEY (thread_id, checkpoint_ns, checkpoint_id, writer_task_id, write_index),
  FOREIGN KEY (thread_id, checkpoint_ns, checkpoint_id)
    REFERENCES workflow_checkpoints(thread_id, checkpoint_ns, checkpoint_id)
    ON DELETE CASCADE
);
