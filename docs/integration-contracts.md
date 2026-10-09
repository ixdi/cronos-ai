# Agent integration contracts

This records the locally installed integration surfaces checked for the MVP (Pi 1.1.0 and Herdr 0.9.3; Herdr API schema protocol 22). No live agent was started during this check.

## Pi RPC

Pi is the workflow orchestrator for the active task. Start a long-lived `pi --mode rpc --no-session` process with only the project-local workflow extension; disable extension discovery, built-in tools, MCP, skills, and project context files so the model sees only the explicit Cronos tools. Rust remains the source of truth for project/task state and human controls. Give Pi the validated task worktree as its working directory. Communicate through newline-delimited JSON on stdin/stdout; stdout is protocol-only and diagnostics go to stderr. Read stdout continuously and correlate command responses with request IDs.

- Submit work with `prompt`; a successful response means accepted/queued, not completed.
- Subscribe to session events before prompting and wait for `agent_settled`, not only `agent_end`, before advancing the workflow. Use `get_last_assistant_text` (or the final message event) for the result.
- Inspect assistant `stopReason`; `error` and `aborted` are failures even when a JSON-mode process exits with status zero.
- Cancel active work with RPC `abort`, which waits for idle. Close stdin for orderly process shutdown. If queued follow-ups exist, clear the queue before aborting when they must not run.

The installed CLI advertises `--mode rpc`, `--no-session`, and the isolation flags used by the launcher; the protocol details above come from the installed Pi RPC documentation. `extensions/cronos-workflow.ts` exposes read-only `cronos_task_state` and `cronos_run_step`. The latter passes the Pi-created agent plan to `cronos-ai internal workflow run-step`; Rust validates status, step order, approvals, agent inputs, and the task worktree before Herdr starts agents. The extension's `session_start` handler submits the assigned task prompt when the host supplies `CRONOS_PROJECT_ID` and `CRONOS_TASK_ID`. Task status and approval writes remain on the Rust host so a model cannot bypass gates. A live model invocation remains an integration-test responsibility.

## Herdr agent API

Herdr 0.9.3 exposes agent operations through its socket API and CLI. The installed API schema advertises `agent.start`, `agent.prompt`, `agent.wait`, `agent.read`, and `agent.send_keys`.

- `agent.start` requires a supported agent kind and an existing pane at an interactive shell prompt. For Pi, use kind `pi`; readiness is reported only after Herdr detects the expected agent in that pane.
- `agent.prompt` submits work; `agent.wait` can observe `working`, `idle`, `blocked`, or `done`; `agent.read` retrieves terminal output.
- Interrupt an active agent with `agent.send_keys` using `ctrl+c`, then observe the agent state. This is terminal-level cancellation and is best-effort; do not treat the key submission alone as proof that the agent stopped. Ignore late results after the workflow has entered Error.
- Workflow-time agent operations run in a Herdr-managed pane with caller context (`HERDR_ENV=1`). Direct TUI startup does not create panes. The explicit `scripts/cronos-ai` launcher is the opt-in exception: outside Herdr it creates a dedicated workspace and pane in the named `cronos-ai` session, runs Cronos-AI there, and never replaces or targets unrelated panes; inside Herdr it uses the current pane.
- The orchestrator calls `cronos_agent_profiles` to discover user-configured profile IDs allowed for the current step; `cronos_run_step` can assign one profile per agent. Rust validates those assignments before recording activity, and Herdr forwards explicit skill/extension/MCP config arguments to each Pi worker without shell interpolation. See [workflow-agent profiles](agent-profiles.html) for setup.

CLI help and the installed API schema confirm these command/parameter contracts. The Rust adapter invokes Herdr with argument vectors (never a shell), validates agent names, parses pane IDs from Herdr responses, and waits for a settled state after sending `ctrl+c`. A workflow step creates no-focus sibling panes only after explicit task start, and only in the task's validated worktree under the Cronos data directory. Startup preflight checks `pi`, `herdr`, `openspec`, and `git` by running their version commands; if any are unavailable it prompts for manual installation and retries, but never installs tools. Only the explicit `scripts/cronos-ai` launcher creates its dedicated Herdr workspace, avoiding existing user panes. Automated launcher tests use fake Herdr commands; live pane/agent launch, prompt, output, and cancellation checks must run in an explicitly isolated test session, not the current user session.

## Git worktrees and local merge

Projects store the canonical root of a user-selected local Git repository. When a task is started, Rust creates an isolated worktree under the Cronos data directory and a branch named `cronos-ai/<task-id>/attempt-<n>`. Recovery removes the exact validated prior-attempt worktree and branch before assigning a fresh attempt. Final-code approval gates the documentation step, which commits all implementation and documentation changes; after Pi settles, Rust requires both the base repository and task worktree to be clean and performs a local `git merge --no-ff --no-edit`. On conflict, Rust aborts the merge so the base worktree remains unchanged and marks the task Error. There is no remote-push operation in the adapter; users publish manually.

## Jev

Jev is not installed and is not required by this release. Use the built-in priority/dependency ordering for the MVP. A later change will create a Pi extension that integrates Jev.
