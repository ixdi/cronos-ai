# Cronos Storage

`cronos-storage` owns the shared SQLite database connection and applies versioned migrations when the service opens the database. Queue helpers and the LangGraph checkpoint saver use that same Bun `bun:sqlite` connection; they do not create separate databases or JSON state files.

## Persisted data

- `tasks` stores the durable task description and task status.
- `workflows` stores the current workflow stage/status, error reason, durable workspace identifier/path used to reconcile an interrupted task, and GitHub pull-request number/URL/branch for delivered work.
- `agent_runs` stores per-role status and result/error summaries.
- `agent_run_events` stores role start, progress, completion, and failure events for operator inspection.
- `workflow_checkpoints` and `workflow_checkpoint_writes` store LangGraph checkpoint payloads and pending writes as SQLite blobs. A graph thread ID is the corresponding task ID.
- `schema_migrations` records applied SQL migrations. A partial unique index allows only one in-flight task across `active`, `review_pending`, and `interrupted` statuses.

The checkpoint saver implements LangGraph's `BaseCheckpointSaver` using the checkpointer serializer and Bun's built-in SQLite API. Checkpoint payloads may use LangGraph's typed serialization format inside SQLite; this is not a JSON state-file store. On restart, active tasks are marked `interrupted`; workspace handles and graph checkpoints remain durable so an operator can request explicit resume. Missing or mismatched checkpoint/workspace data leaves the task interrupted rather than silently starting a replacement workspace.

## Database lifecycle

Open the database once through `openStorageDatabase(path)`, pass that connection to storage, queue, and checkpoint components, and close it when the owning service shuts down. Migrations are applied transactionally and are safe to rerun. Keep the database file and any credentials out of version control.

Run storage tests with:

```sh
bun run --filter cronos-storage test
```
