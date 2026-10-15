# Cronos-AI: Review of Doubts, Incoherences, and Improvements

This review covered the design docs, diagrams, OpenSpec specs, guides, the Pi extension, and the core Rust workflow. The findings below were resolved by the OpenSpec change `harden-workflow-execution`. A second code review of that change found further gaps, listed under "Second review"; they were fixed in the same change before it was archived. The current behavior is defined by `openspec/specs/` and described in `design/design.md`.

## Problems that would stop real use

| #   | Finding                                                                                           | Resolution                                                                                                                                                                                                                                       |
| --- | ------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| 1   | Agents had 2- and 3-minute time limits (Herdr prompt wait, extension tool call).                  | Per-step agent limits: 30 or 60 minutes, overridable by profile `timeout_minutes`. The extension call has no client-side timeout. The TUI shows step elapsed time.                                                                               |
| 2   | A step passed when agents went idle; results were scraped terminal lines; nothing enforced tests. | Agents write structured JSON results (`success` / `changes_requested` / `failed`), and Rust decides from them. Rust runs the project check command after verification. A failure starts an automatic revision.                                   |
| 3   | Parallel agents shared one worktree with no rules.                                                | Specification and code generation use exactly 1 agent, which commits. Review and verification (1–4 agents) are read-only, and Rust detects changes. Documentation agents (1–4) don't commit; Rust archives the change and makes one docs commit. |
| 4   | Humans approved without seeing the work.                                                          | Task details show the worktree, branch, OpenSpec change, and elapsed time. The review view (`v`) shows the spec, commits, diff summary, and check output.                                                                                        |
| 5   | An interrupted task blocked the queue forever.                                                    | `s` resumes an In Progress task with no orchestrator. It cancels unfinished agents, then retries the merge or restarts Pi at the current step.                                                                                                   |
| 6   | Every failure discarded approved specs and code.                                                  | Retry step (`t`) keeps the attempt, worktree, and approvals. A merge conflict returns to code generation. A dirty base makes the merge wait instead of failing.                                                                                  |
| 7   | "Models cannot bypass gates" overstated the protection.                                           | The docs now state the trust boundary: Cronos tools cannot change state, but a worker's shell could. A dedicated user or container is recommended. The orchestrator has only `read` plus the Cronos tools.                                       |
| 8   | The release binary depended on its source checkout.                                               | The extension is embedded in the binary and written to the data directory at launch.                                                                                                                                                             |

## Design, spec, and code disagreements

Rule applied: the specs and code are the truth. `design/design.md` now labels every feature as Implemented or Future.

| Topic                                 | Resolution                                                                                                                                                           |
| ------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Push to GitHub                        | Future; the MVP merges locally only.                                                                                                                                 |
| HTML real-time dashboard              | Future; the TUI is the dashboard.                                                                                                                                    |
| Project definition from requirements  | Implemented: a read-only Pi planner proposes tasks from the project's requirements; a human applies or discards the plan.                                          |
| Step list numbering and scope         | Project-level steps (collection, prioritization) are separated from the 8 per-task steps. The duplicate "3." is removed. Prompt generation is part of orchestration. |
| Model per step                        | Implemented through profile `model`.                                                                                                                                 |
| Release number and changelog per task | Future, as per-release versioning, so parallel tasks don't conflict.                                                                                                 |
| Kanban and analytics                  | Future.                                                                                                                                                              |
| Jev                                   | Future; built-in priority and dependency ordering is used.                                                                                                           |

## OpenSpec questions

| Question                                     | Resolution                                                                                                                                                             |
| -------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Cronos's own specs were unversioned.         | `openspec/config.yaml` and `openspec/specs/` are tracked; working changes stay local.                                                                                  |
| Target repositories might not have OpenSpec. | Starting a task requires a committed `openspec/`; otherwise the worktree is discarded with a clear error.                                                              |
| How do tasks map to changes?                 | One change per task, named `cronos-<first 8 hex of task id>`.                                                                                                          |
| When do specs sync?                          | During documentation, `openspec archive` syncs the specs on the task branch, so they land with the merge. Conflicting capability edits follow the merge-conflict path. |

## Open questions and decisions

1. **Is the Pi orchestrator worth having?** Yes. AGENTS.md requires it, and it writes task-specific prompts, chooses agents and profiles, and passes findings to revisions. It can now read the worktree. Behavior the gates depend on is no longer left to the model: Rust appends a fixed contract to every agent prompt.
2. **What makes an agent fail?**
   - A crash, a timeout, a blocked prompt, a missing or invalid result, or a `failed` result: the task goes to Error.
   - `changes_requested` from review or verification, or a failing check command: the task loops back to code generation automatically, at most 3 times per attempt.
3. **Which part is the product?** The per-task supervised pipeline is the core product; requirement-driven planning feeds it reviewed tasks.
4. **Is a clean base repository required?** Yes, but only at the moment of the merge. A dirty base makes the task wait at local merge; pressing `s` retries.

## Suggested improvements

| #   | Improvement                                                             | Status                                                                      |
| --- | ----------------------------------------------------------------------- | --------------------------------------------------------------------------- |
| 1   | Structured agent results and an enforced check command                  | Done                                                                        |
| 2   | Per-step time limits and elapsed time                                   | Done                                                                        |
| 3   | Safe parallel work (single writers, read-only checks, Rust docs commit) | Done                                                                        |
| 4   | Show the work at approval time                                          | Done (review view)                                                          |
| 5   | Resume and retry a single step; handle merge conflicts                  | Done                                                                        |
| 6   | Honest trust boundary                                                   | Done (documented; the orchestrator is limited to read-only access)          |
| 7   | Align the design doc with the specs                                     | Done                                                                        |
| 8   | Profile `model`                                                         | Done (plus `timeout_minutes`)                                               |
| 9   | Version OpenSpec specs; one change per task                             | Done                                                                        |
| 10  | Live smoke test                                                         | Done: `tests/live_smoke.rs`, opt-in with `CRONOS_LIVE_SMOKE=1` inside Herdr |

## Second review

| #   | Finding                                                                                                                                      | Resolution                                                                                                     |
| --- | -------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------- |
| 1   | Every merge failure was treated as a conflict and looped through code generation.                                                            | Only unmerged paths are a conflict; any other failure is aborted and sets Error.                               |
| 2   | Sibling cancellation was not sticky: an agent still launching could start and run its full limit.                                            | The adapter remembers cancellations and checks before every launch stage.                                      |
| 3   | Agents of a crashed orchestrator kept running; `t` reused their names and `r` deleted their worktree.                                        | Agent names include the attempt. Recovery stops stale agents and closes their panes first.                     |
| 4   | Two step processes could run for one task, and a stale one could still write results.                                                        | A per-task OS lock, held by `run-step`, gates recovery; a step discards results if the task changed meanwhile. |
| 5   | Herdr panes were never closed.                                                                                                               | Every agent pane is closed when the agent ends.                                                                |
| 6   | The check log followed symlinks; result files could be removed through a symlinked directory; a check timeout left child processes running.  | Symlink-safe file and directory handling; the check runs in its own process group.                             |
| 7   | The check program can be edited by the agent it checks, and build output could end up in the docs commit.                                    | A changed check program is flagged to the reviewer; files left by the check send the task back.                |
| 8   | Findings could be cut from the orchestrator's view; the revision-limit stop had no recorded reason; TUI errors vanished on the next refresh. | Current-attempt activity is passed; the reason is recorded; errors stay until the next key press.              |
| 9   | `openspec/` was still git-ignored, the user docs were not updated, and several spec scenarios had no test.                                   | Specs are tracked, the docs are updated, and the missing tests (including a CLI e2e test) were added.          |

## Remaining limits

- The optional write sandbox protects Cronos data, the base checkout, and Git hooks, but not network access or reads, and agents can still weaken tests in their worktree; the human gates are the real protection.
- The live smoke test must be run manually in an isolated Herdr session.
