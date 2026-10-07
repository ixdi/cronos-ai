DROP INDEX tasks_single_active_idx;
CREATE UNIQUE INDEX tasks_single_inflight_workflow_idx ON tasks ((1))
WHERE status IN ('active', 'review_pending', 'interrupted');
