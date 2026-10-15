# Cronos-AI - AI Software Factory

Cronos-AI is an opinionated, human-supervised software factory. It takes the tasks of a project through an ordered workflow of AI agents and human approval gates, and produces committed, verified, and documented changes in a local Git repository.

- **Rust** owns persisted state, validated transitions, human approvals, and all Git and OpenSpec bookkeeping.
- **Pi** orchestrates the active task.
- **Herdr** runs the worker agents.
- **OpenSpec** holds each task's specifications.

The OpenSpec specs in `openspec/specs/` are the source of truth for behavior. This document explains the design and labels each feature as **Implemented** (in the MVP) or **Future** (designed but not built). Diagrams:
- `design/architecture.mmd`, rendered as `design/LLD_mermaid.png`: the as-built architecture.
- `design/workflow.drawio`, `design/workflow.svg`, `design/workflow.png`: the per-task step sequence inside the future parallel feature-track model.

## Execution model

- **Implemented:**
  - Up to the project's parallel capacity (1 by default, at most 8) of tasks run at the same time, each on its own feature track: worktree, branch, step lock, Herdr agents, and Pi orchestrator. Tasks in Error keep their track until recovered or abandoned.
  - Scheduling is conflict-aware: a ready task that shares a requirement with an active task waits. Dependencies still order related work, store changes and merges are serialized, and a merge conflict returns the later task to code generation.
  - Within a task, steps run sequentially; a step starts only after the previous one finishes.
  - Independent agents within one step run in parallel.

## Task statuses

| Status | Meaning |
|---|---|
| Pending | Waiting in the ready queue (priority, then creation time; dependencies must be Completed). |
| In Progress | The workflow is running, waiting at an approval gate, waiting for a clean base repository at local merge, or interrupted and resumable. Approval rejections and automatic revisions keep this status. |
| Completed | Final approval, documentation, and the local merge succeeded. |
| Rejected | A human explicitly abandoned the whole task. Rejecting an approval step is a revision, not Rejected. |
| Error | The workflow stopped. Running sibling agents were cancelled, and a human chooses step retry, retry from specs, clarification, or abandonment. |

## Technologies

| Component | Role | Status |
|---|---|---|
| Rust CLI + Ratatui TUI | Projects, tasks, monitoring, approvals, recovery, review view | Implemented |
| Pi (RPC mode) | Orchestrator with the read-only `read` tool and the Cronos workflow tools only | Implemented |
| Embedded Pi extension | `cronos_task_state`, `cronos_agent_profiles`, `cronos_run_step`; written to the data directory at launch | Implemented |
| Herdr | Runs each worker as a Pi agent in its own pane; prompt, wait with time limit, read, cancel | Implemented |
| OpenSpec | One change per task; validated after specs and archived during documentation | Implemented |
| Git worktrees | One worktree and branch per attempt; local merge only | Implemented |
| Agent profiles | Per-step Pi skills, extensions, MCP config, model, and time limit; managed in the TUI (`p`) or by editing `agent-profiles.json` | Implemented |
| Built-in step skills | One Pi skill per agent step (`skills/`), embedded in the binary and loaded by every worker before profile skills | Implemented |
| Jev | Prioritization and dependency model | Future (built-in priority/dependency ordering is used) |
| Remote push / GitHub | Publishing after merge: local only, merge and push, or pull request through `gh` | Implemented |
| Release numbers, changelog per release | Opt-in per project: each local merge is tagged with the next semantic version from its Conventional Commits and prepended to `CHANGELOG.md`; computed on the base at merge time so parallel tasks never conflict | Implemented |

## Projects

- **Implemented:**
  - Create, list, select, update, and archive projects.
  - A project stores a local Git repository and a **check command**: a program and arguments run without a shell, for example `./scripts/check.sh`. Both are required before tasks start.
  - The repository must contain a committed `openspec/` directory, created with `openspec init`.
- **Implemented, project definition:** the project stores product requirements (functional and non-functional). A read-only Pi planner (`project plan`, or `p` on the TUI requirements screen) reads the repository and its OpenSpec specs with the embedded `cronos-project-planning` skill and proposes tasks covering every requirement, with priorities, dependencies, and requirement links. Rust validates the plan and keeps it pending; a human applies or discards it. Each planned task then gets its own OpenSpec change in specification generation.

## Workflow

### Project-level steps

1. **Task collection.**
   - *Implemented:* tasks are entered in the TUI or CLI, or planned from requirements. Requirement and task text is normalized (trimmed, whitespace collapsed); a duplicate requirement or a task duplicating an open task is rejected.
   - *Future:* more input sources, such as issue trackers.
2. **Task prioritization.**
   - *Implemented:* priority (high, medium, low), dependencies within the same project, and a ready queue that never starts a task before its dependencies are Completed.
   - *Future:* Jev-based ranking.

### Per-task steps

Each step lists its agents and the rules Rust enforces.

| # | Step | Agents | Rust-enforced rules |
|---|---|---|---|
| 1 | `specification-generation` | exactly 1 | The agent writes OpenSpec change `cronos-<task-id prefix>` and commits it. The worktree must be clean. `openspec validate --strict` must pass. |
| 2 | `specification-approval` | human | Approve to continue to step 3, or request a revision with a note to return to step 1. |
| 3 | `code-generation` | 1-4 | One agent implements the change with tests and commits (Conventional Commit) in the task worktree, which must be left clean. Several agents each work on their own part in a separate worktree and branch (`attempt-<n>-agents/<agent>`); Rust merges their branches into the task branch in agent-name order. A conflict resets the task branch and sends the task back to code generation. |
| 4 | `code-review` | 1-4 in parallel | Read-only: Rust compares `HEAD` and file status before and after the step. |
| 5 | `verification` | 1-4 in parallel | Read-only. Afterwards, Rust runs the project check command in the worktree (the project's check time limit, 60 minutes by default and 1 to 480 when set; the whole process group is stopped on timeout). The check must not leave new files in the worktree. |
| 6 | `final-approval` | human | Approve to continue to step 7, or request a revision with a note to return to step 3. |
| 7 | `documentation` | 1-4 in parallel | Agents write documentation files without committing. Rust archives the OpenSpec change (syncing its specs) and makes one `docs: document <title>` commit. |
| 8 | `local-merge` | Rust | By the project's publish mode: `local` requires a clean base and worktree and runs `git merge --no-ff`; `push` then pushes the base branch; `pull-request` pushes the task branch and opens a pull request with `gh` instead of merging. Every stage is idempotent, so a failed publish is retried with a step retry. |

**Prompts.** Pi writes each agent's task-specific prompt. It can read the worktree to do this, but cannot change it. Rust appends a fixed **contract** to every prompt, which states:
- the result file;
- the OpenSpec change name;
- the step's write and commit rules;
- that agents must never push or edit Cronos state.

**Results.** Every agent ends by writing `.cronos/results/<step>/<agent>.json`:

```json
{"status": "success" | "changes_requested" | "failed", "summary": "...", "findings": ["..."]}
```

Rust decides the step outcome from these files, never from terminal output. `.cronos/` is listed in the repository's local `info/exclude`, so result files and check output are never committed.

**Time limits.**
- Defaults: 30 minutes for specification generation, review, and documentation; 60 minutes for code generation and verification.
- A profile's `timeout_minutes` overrides the default for agents using that profile.
- The TUI shows how long the current step has been running.

### Failures and automatic revisions

| Event | Result |
|---|---|
| Agent crash, Herdr error, time limit exceeded, blocked prompt, or missing, invalid, or `failed` result | Cancel siblings (cancellation is sticky, so a sibling that has not started never starts), close every pane the step opened, set **Error** |
| A read-only step changed the worktree, a writer left uncommitted work, or OpenSpec validation or archive failed | **Error** |
| `changes_requested` from review or verification, a failing check command, or a check that leaves new files in the worktree | Record the findings, clear the attempt's final-code approval, and return to `code-generation` (**In Progress**) |
| More than 3 automatic revisions in one attempt | **Error**, with the reason recorded in agent activity |
| Merge conflict | Abort the merge, record the conflict, and return to `code-generation` so the agent merges the base branch and resolves it (counts as an automatic revision) |
| Any other merge failure (no unmerged paths, for example a failing Git hook) | Abort the merge and set **Error** |
| Base repository dirty at merge time | Stay at `local-merge`; the user cleans up and presses `s` |
| The task branch changed the check program | The check runs that version; a warning is recorded and shown at the top of the review view |

Pi keeps running agent steps until the task reaches an approval gate, local merge, Error, or Completed. After an approval, the TUI starts Pi again.

### One step process per task

- `run-step` holds an exclusive per-task OS file lock (`<data>/locks/<task-id>.lock`) for the whole step. The OS releases it when the process exits, even after a crash.
- Resume, step retry, retry, and clarification refuse to act while the lock is held. Once it is free, they stop agents the dead process left running (found through `herdr agent get`) and close their panes.
- After its agents finish, a step reloads the task. If the attempt, step, or status changed meanwhile, it discards its results instead of writing them.
- Herdr agent names include a stable FNV-1a hash of the task ID, attempt, and agent name, so a new attempt never reuses an older attempt's agents.

### Human recovery

| Action | Effect |
|---|---|
| Retry step (`t`) | Error becomes In Progress at the failed step, keeping the attempt, worktree, and approvals. Use it for transient failures. Like `r`, `c`, and `s`, it first stops stale agents and waits for any running step process. |
| Retry (`r`) | Discards the attempt's worktree and branch and starts a new attempt at specification generation. Downstream outputs are not reused. |
| Clarify (`c`) | Like Retry, plus context that the new specifications must apply. The new specifications need approval again. |
| Resume (`s`) | For an In Progress task with no running orchestrator (for example, after the TUI was closed). Marks unfinished agents as cancelled, then retries the merge or restarts Pi at the current step. |
| Abandon (`a`) | Sets Rejected and records a reason. |

## OpenSpec usage

- **One change per task.** Each task's change is named `cronos-` followed by the first 8 hex characters of the task UUID. Revisions and step retries update the same change; a new attempt starts again from the base branch.
- **Spec sync timing.** Documentation archives the change on the task branch, so the synced main specs arrive in the base branch with the local merge. If two tasks change the same capability, the merge conflicts and follows the conflict path above.
- **This repository's own specs.** Cronos-AI tracks `openspec/config.yaml` and `openspec/specs/` in Git. Working changes (`openspec/changes/`) stay local.

## Dashboard (TUI)

- **Implemented:**
  - Project list and creation form (name, description, repository, check command, check time limit, publish mode, releases).
  - Task list and creation form (title, description, priority, dependencies).
  - Task details: status, step, elapsed time, attempt and automatic-revision count, worktree, branch, OpenSpec change, approvals, recoveries, and agent activity.
  - Task list as a table or a kanban board by status (`b`), with a progress summary: done out of not abandoned, average completion time, and errors.
  - Review view (`v`): the OpenSpec change, task-branch commits, diff summary against the base, the latest check output, and the per-file diff with file jumps (`]`/`[`).
  - Approvals and revision notes, recovery actions, and abandonment.
  - Agent profiles screen (`p`): list, create, edit, and delete profiles; each resource path is checked, and a broken catalog can be opened and fixed.
  - A semantic color palette that keeps text labels.
  - A one-second refresh.
- **Future:**
  - A web dashboard.

## Storage

- **Projects:** `projects/<id>/project.json` holds the project definition, requirements, repository, check command, publishing settings, and task IDs; `projects/<id>/plan.json` holds a plan waiting for approval.
- **Tasks:** `projects/<id>/tasks/<task-id>.json` holds:
  - the task fields and the step timestamps;
  - bounded agent activity, approval, and recovery histories;
  - the attempt number and the automatic-revision count;
  - the worktree path.
- **Writes and validation:** writes are atomic; records are validated when loaded; symlinks and oversized files are rejected.
- **Concurrent changes:** every change reads, checks, and saves its records under an exclusive store lock (`<data>/locks/store.lock`), so the TUI, the CLI, and a running step never overwrite each other. When a task's step lock is also needed, it is taken first.
- **Code:** never stored in JSON. It lives in the attempt worktree under the data directory.
- **Future:** project requirements, prompts, and resource snapshots per task.

## Trust boundary

- **What Rust enforces:** Cronos tools cannot change task status, approvals, or worktree paths. Rust validates every step, profile, and transition, and checks worktree rules after each step.
- **What it cannot enforce:** workers are full Pi sessions with shell access, running with the user's operating-system permissions. A worker's shell could edit Cronos data files, leave the worktree, weaken tests or the check program, or push. Cronos never writes or deletes through symlinks agents place in `.cronos/`, and it flags a changed check program, but the human gates and the review view are the real protection.
- **Optional write sandbox (implemented):** With `project set-sandbox on`, each worker runs in a write sandbox (`sandbox-exec` on macOS, bubblewrap on Linux): it can still write its worktree, the repository's Git objects, build caches, and temporary files, but not Cronos data, the base checkout, or the repository's `.git/hooks` and `.git/config`. The sandbox does not limit network access or reading files, and a worker can still weaken tests inside its worktree, so the human gates remain the real protection. Herdr replaces the worker pane's shell with a sandboxed shell before Pi starts, and a sandboxed project refuses to run when the sandbox tool is missing.
- **Recommendation:** run Cronos-AI as a dedicated OS user or in a container for untrusted work.

## Future work

- Jev-based prioritization.
