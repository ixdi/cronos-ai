# Cronos Core

## Local task intake (MVP)

`createIntakeServer(db, port)` exposes `POST /tasks` on `127.0.0.1` only. It does not bind to a network-facing interface. The core service should open the shared SQLite database with `openStorageDatabase(path)`, pass that connection to the server, and close both during shutdown.

The intake accepts a JSON object with a non-empty `description` string, trims surrounding whitespace, persists the task through `cronos-queue`, and returns HTTP `201` with the queued item:

```sh
curl --fail-with-body \
  -X POST http://127.0.0.1:3000/tasks \
  -H 'content-type: application/json' \
  -d '{"description":"Implement the first feature"}'
```

Example response:

```json
{
  "id": "<task-id>",
  "description": "Implement the first feature",
  "status": "queued",
  "createdAt": "<timestamp>",
  "updatedAt": "<timestamp>"
}
```

The server returns `400` for malformed JSON or missing/empty descriptions, `413` when the request body exceeds 64 KiB, and `415` when the request is not JSON. Invalid submissions are not queued.

This endpoint is intended for local clients in the first slice. Do not expose it through a network interface or reverse proxy; network access requires a separate authentication and rate-limiting design.

The Bun test suite exercises the same `POST /tasks` request against an ephemeral loopback server:

```sh
bun run --filter cronos-core test
```

## Workflow stages and task states

`processNextTask` claims at most one queued task, provisions its task workspace, and runs the LangGraph workflow. The persisted `workflows.current_stage` follows these executable stages:

| Stage | Transition |
| --- | --- |
| `triage` | Jev classifies the task. `auto_resolvable` continues; clarification/specification routes become `blocked` without implementation. |
| `implementation` | Pi changes files in the task worktree. A human rejection returns here with the review feedback. |
| `code_review` | Read-only code review runs; a failed quality gate ends in `errored`. |
| `verification` | Verification runs; a failed gate ends in `errored`. |
| `final_review` | The task becomes `review_pending` until an explicit human decision. Rejection loops to implementation; approval advances to documentation. |
| `documentation` | Pi updates task documentation after approval. |
| `pull_request` | The LangGraph graph ends after documentation. With a configured `pullRequestCreator`, the core stages a host-owned commit, pushes a branch, creates the PR, and atomically persists its reference with the delivered status. Without one it returns `ready_for_delivery` and leaves the task active. |
| `delivered` | GitHub confirmed the PR; its number, URL, and branch are stored. The workspace is closed and no later PR/CI events are tracked. |
| `blocked` / `errored` | Terminal task outcomes for unsupported triage routes or failed workflow/quality gates. |
| `interrupted` | Recovery status for work that was active when Cronos restarted; `current_stage` retains the last persisted stage. |

Task status is stored separately from workflow stage: `queued`, `active`, `review_pending`, `blocked`, `errored`, `interrupted`, and `delivered`. Active, review-pending, and interrupted tasks hold the single-workflow lock. Blocked and errored tasks release it. A blocked task retains its reason in the workflow record; its implementation stage is never entered.

## Restart and explicit resume

Call `reconcileActiveTasks` during service startup before claiming queued work. It marks each previously `active` task `interrupted`, records why, and asks the workspace provider to stop its Herdr/Docker runtime. It does not resume agents. The SQLite checkpoint and task worktree are preserved; a `review_pending` task remains pending for its human decision rather than being reclassified as interrupted.

`resumeInterruptedTask(dependencies, taskId)` is the explicit resume operation. It requires both a LangGraph checkpoint and a matching SQLite-persisted workspace handle, reserves the interrupted task before restoring resources, and invokes the graph from its latest checkpoint. The Herdr provider creates a fresh Herdr/Docker runtime around the existing task worktree; it does not create a new worktree. If the checkpoint, workspace, or runtime cannot be restored, the operation returns `resume_failed` and leaves the task interrupted with an explanatory workflow error. A failure after execution resumes is recorded as `errored`.

The operator CLI exposes `list`, `show`, `resume`, `approve`, and `reject` through the `cronos-panel` Bun package. See `cronos-panel/README.md` for command syntax and runtime environment configuration, and `docs/github-app-setup.md` for GitHub App permissions and secret setup.
