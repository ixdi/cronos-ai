# Workflow state and execution

## Task queue

Cronos runs up to the project's parallel capacity of tasks at a time (1 by default; set 1 to 8 with `project set-parallelism` or the project form). Tasks that are In Progress or Error fill the tracks, so an Error task must be recovered or abandoned to free its track. `start_next_task` selects the highest-priority ready Pending task whose dependencies are Completed and that shares no requirement with an active task; a task that would touch the same requirement waits. With `project set-prioritizer agent`, a fast agent first ranks the startable tasks and they start in its order; it cannot start a task the rules disallow, and the built-in order is used when it fails. In the TUI, **s** resumes every interrupted task and then starts ready tasks until the tracks are full, each with its own Pi orchestrator.

A project needs a local Git repository with a committed `openspec/` directory (`openspec init`, then commit it) and a check command before its tasks can start.

## Ordered steps

| # | Step | Agents | What Rust enforces |
|---|---|---|---|
| 1 | `specification-generation` | exactly 1 | The agent writes and commits OpenSpec change `cronos-<first 8 hex of the task ID>`. The worktree must be clean and `openspec validate <change> --strict` must pass. |
| 2 | `specification-approval` | human | Approve to continue, or request a revision with a note to return to step 1. |
| 3 | `code-generation` | 1-4 | One agent implements the change with tests and commits; the worktree must be clean. Several agents each commit their own part in a separate worktree, and Rust merges their branches in agent-name order; a conflict resets the task branch and returns the task to code generation. |
| 4 | `code-review` | 1-4 in parallel | Read-only: the commit and file status must not change. |
| 5 | `verification` | 1-4 in parallel | Read-only. Then Rust runs the project check command in the worktree. |
| 6 | `final-approval` | human | Approve to continue, or request a revision with a note to return to step 3. |
| 7 | `documentation` | 1-4 in parallel | Agents edit documentation without committing. Rust archives the OpenSpec change and makes one `docs: document <title>` commit. |
| 8 | `local-merge` | Rust | Publishes by the project's mode: `local` requires a clean base and worktree and runs `git merge --no-ff`; `push` also pushes the base branch to the remote; `pull-request` pushes the task branch and opens a pull request instead of merging. With releases on, a local merge is tagged as the next version and added to `CHANGELOG.md`. |

Steps run in order; agents within one step run in parallel and the step waits for all of them.

A failed push or pull request is recorded in the task's activity and stops the task with **Error**. Fix the cause (for example the remote's credentials; Git and `gh` never prompt), then retry the step with **t**: a merge that already happened is not repeated, a release already tagged on HEAD is reused, and an open pull request is reused. Steps that need a human cannot be advanced by an agent tool.

## Agent results and time limits

Rust appends a fixed contract to every agent prompt: the result file, the OpenSpec change, the step's write rules, and that agents never push or edit Cronos state. Every agent ends by writing `.cronos/results/<step>/<agent>.json`:

```json
{"status": "success" | "changes_requested" | "failed", "summary": "...", "findings": ["..."]}
```

Rust decides the step outcome from these files, never from terminal output. `.cronos/` is listed in the repository's local `info/exclude`, so result files and check output are never committed. Cronos refuses to read or remove result files through a symlinked directory.

Agents run under a time limit: 30 minutes for specification generation, code review, and documentation; 60 minutes for code generation and verification. A profile's `timeout_minutes` overrides it. The check command runs under the project's check time limit, 60 minutes unless the project sets 1 to 480 minutes with `project set-check-timeout` or the TUI project form; on timeout its whole process group is stopped.

## Outcomes

| Event | Result |
|---|---|
| An agent crashes, exceeds its limit, blocks on a prompt, or leaves a missing, invalid, or `failed` result | Siblings are cancelled (including any that had not started yet), every pane the step opened is closed, and the task becomes **Error** |
| A read-only step changed the worktree, a writer left uncommitted work, or OpenSpec validation or archive failed | **Error** |
| `changes_requested` from review or verification, a failing check command, or a check that leaves new files in the worktree | Findings are recorded, final-code approval for the attempt is cleared, and the task returns to `code-generation` (**In Progress**) |
| More than 3 automatic revisions in one attempt | **Error**, with the reason recorded in agent activity |
| Merge conflict | The merge is aborted, the conflict is recorded, and the task returns to `code-generation` to merge the base branch and resolve it (counts as an automatic revision) |
| Any other merge failure (for example a failing Git hook) | The merge is aborted and the task becomes **Error** |
| Base repository dirty at merge time | The task waits at `local-merge`; clean the base and press **s** |

If the task branch changes the check program itself (for example `./scripts/check.sh`), the check runs the agent's version. Cronos records a warning in the check-command activity and at the top of the review view so the human sees it before final approval.

## One step process per task

`cronos-ai internal workflow run-step` holds an exclusive per-task lock (`locks/<task-id>.lock` in the data directory) while its agents run. The operating system releases the lock when the process exits, even after a crash. Resume, step retry, retry, and clarification refuse to act while the lock is held. After the lock is free, they first stop any agents the dead process left running and close their panes. A step that finishes after the task was changed by a human (another attempt, a different step, or no longer In Progress) discards its results. Herdr agent names include the attempt, so a new attempt never reuses an older attempt's agents.

## Recovery

| Action | Key | Effect |
|---|---|---|
| Retry step | **t** | Error becomes In Progress at the failed step, keeping the attempt, worktree, and approvals. Use it for transient failures. |
| Retry | **r** | Discards the attempt's worktree and branch and starts a new attempt at specification generation. Downstream outputs are not reused. |
| Clarify | **c** | Like Retry, plus context that the new specifications must apply. The new specifications need approval again. |
| Resume | **s** | For an In Progress task with no running task runner (for example, after the TUI was closed). Cancels unfinished agents, then retries the merge or restarts Pi at the current step. |
| Abandon | **a** | Sets Rejected and records a reason. |

Agent, approval, and recovery history stays tagged by attempt; the removed worktree path is kept in recovery history.
