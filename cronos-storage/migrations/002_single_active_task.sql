CREATE UNIQUE INDEX tasks_single_active_idx ON tasks(status) WHERE status = 'active';
