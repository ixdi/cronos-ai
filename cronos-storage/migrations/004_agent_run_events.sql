CREATE TABLE agent_run_events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  agent_run_id TEXT NOT NULL REFERENCES agent_runs(id) ON DELETE CASCADE,
  type TEXT NOT NULL CHECK (type IN ('started', 'progress', 'completed', 'failed')),
  message TEXT NOT NULL,
  created_at TEXT NOT NULL
);

CREATE INDEX agent_run_events_run_created_idx ON agent_run_events(agent_run_id, created_at, id);
