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
  - One task runs at a time.
  - Within a task, steps run sequentially; a step starts only after the previous one finishes.
  - Independent agents within one step run in parallel.
- **Future:** several tasks run in parallel on separate feature tracks. The step order within each task does not change.

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
| Jev | Prioritization and dependency model | Future (built-in priority/dependency ordering is used) |
| Remote push / GitHub | Publishing after merge | Future (users push manually) |
| Release numbers, changelog per release | Versioning of merged work | Future |

## Projects

- **Implemented:**
  - Create, list, select, update, and archive projects.
  - A project stores a local Git repository and a **check command**: a program and arguments run without a shell, for example `./scripts/check.sh`. Both are required before tasks start.
  - The repository must contain a committed `openspec/` directory, created with `openspec init`.
- **Future, project definition:** the project stores product requirements (functional and non-functional). A Pi agent uses OpenSpec explore to turn them into specifications and generates one task per requirement. Today, users write tasks by hand.

## Workflow

### Project-level steps

1. **Task collection.**
   - *Implemented:* tasks are entered in the TUI or CLI.
   - *Future:* cleaning, deduplication, normalization, and more input sources.
2. **Task prioritization.**
   - *Implemented:* priority (high, medium, low), dependencies within the same project, and a ready queue that never starts a task before its dependencies are Completed.
   - *Future:* Jev-based ranking.

### Per-task steps

Each step lists its agents and the rules Rust enforces.

| # | Step | Agents | Rust-enforced rules |
|---|---|---|---|
| 1 | `specification-generation` | exactly 1 | The agent writes OpenSpec change `cronos-<task-id prefix>` and commits it. The worktree must be clean. `openspec validate --strict` must pass. |
| 2 | `specification-approval` | human | Approve to continue to step 3, or request a revision with a note to return to step 1. |
| 3 | `code-generation` | exactly 1 | The agent implements the change with tests and commits (Conventional Commit). The worktree must be clean. |
| 4 | `code-review` | 1-4 in parallel | Read-only: Rust compares `HEAD` and file status before and after the step. |
| 5 | `verification` | 1-4 in parallel | Read-only. Afterwards, Rust runs the project check command in the worktree (60-minute limit, whole process group stopped on timeout). The check must not leave new files in the worktree. |
| 6 | `final-approval` | human | Approve to continue to step 7, or request a revision with a note to return to step 3. |
| 7 | `documentation` | 1-4 in parallel | Agents write documentation files without committing. Rust archives the OpenSpec change (syncing its specs) and makes one `docs: document <title>` commit. |
| 8 | `local-merge` | Rust | Requires a clean base and worktree, then runs `git merge --no-ff`. Nothing is pushed. |

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
  - Project list and creation form (name, description, repository, check command).
  - Task list and creation form (title, description, priority, dependencies).
  - Task details: status, step, elapsed time, attempt and automatic-revision count, worktree, branch, OpenSpec change, approvals, recoveries, and agent activity.
  - Review view (`v`): the OpenSpec change, task-branch commits, diff summary against the base, and the latest check output.
  - Approvals and revision notes, recovery actions, and abandonment.
  - Agent profiles screen (`p`): list, create, edit, and delete profiles; each resource path is checked, and a broken catalog can be opened and fixed.
  - A semantic color palette that keeps text labels.
  - A one-second refresh.
- **Future:**
  - A kanban view of task statuses.
  - Analytics such as counts per status, average completion time, and overall progress.
  - Inline diffs per file.
  - A web dashboard.

## Storage

- **Projects:** `projects/<id>/project.json` holds the project definition, repository, check command, and task IDs.
- **Tasks:** `projects/<id>/tasks/<task-id>.json` holds:
  - the task fields and the step timestamps;
  - bounded agent activity, approval, and recovery histories;
  - the attempt number and the automatic-revision count;
  - the worktree path.
- **Writes and validation:** writes are atomic; records are validated when loaded; symlinks and oversized files are rejected.
- **Code:** never stored in JSON. It lives in the attempt worktree under the data directory.
- **Future:** project requirements, prompts, and resource snapshots per task.

## Trust boundary

- **What Rust enforces:** Cronos tools cannot change task status, approvals, or worktree paths. Rust validates every step, profile, and transition, and checks worktree rules after each step.
- **What it cannot enforce:** workers are full Pi sessions with shell access, running with the user's operating-system permissions. A worker's shell could edit Cronos data files, leave the worktree, weaken tests or the check program, or push. Cronos never writes or deletes through symlinks agents place in `.cronos/`, and it flags a changed check program, but the human gates and the review view are the real protection.
- **Recommendation:** run Cronos-AI as a dedicated OS user or in a container for untrusted work.

## Future work

- Parallel tasks on feature tracks, with conflict-aware scheduling.
- Requirement-driven project definition and task generation.
- Jev-based prioritization.
- Optional remote push or PR creation after merge; release numbering; changelog per release.
- Kanban, analytics, and inline diff views in the TUI.
- Parallel code-generation agents in separate sub-worktrees with a deterministic merge.
- A configurable check-command time limit.
- Sandboxed workers.
