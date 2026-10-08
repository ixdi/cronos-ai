# Agent integration contracts

This records the locally installed integration surfaces checked for the MVP (Pi 1.1.0 and Herdr 0.9.3; Herdr API schema protocol 22). No live agent was started during this check.

## Pi RPC

Start a long-lived worker with `pi --mode rpc --no-session` and an explicit task worktree as its working directory. Communicate through newline-delimited JSON on stdin/stdout; stdout is protocol-only and diagnostics go to stderr. Read stdout continuously and correlate command responses with request IDs.

- Submit work with `prompt`; a successful response means accepted/queued, not completed.
- Subscribe to session events before prompting and wait for `agent_settled`, not only `agent_end`, before advancing the workflow. Use `get_last_assistant_text` (or the final message event) for the result.
- Inspect assistant `stopReason`; `error` and `aborted` are failures even when a JSON-mode process exits with status zero.
- Cancel active work with RPC `abort`, which waits for idle. Close stdin for orderly process shutdown. If queued follow-ups exist, clear the queue before aborting when they must not run.

The installed CLI advertises `--mode rpc` and `--no-session`; the protocol details above come from the installed Pi RPC documentation. A live model invocation remains an integration-test responsibility.

## Herdr agent API

Herdr 0.9.3 exposes agent operations through its socket API and CLI. The installed API schema advertises `agent.start`, `agent.prompt`, `agent.wait`, `agent.read`, and `agent.send_keys`.

- `agent.start` requires a supported agent kind and an existing pane at an interactive shell prompt. For Pi, use kind `pi`; readiness is reported only after Herdr detects the expected agent in that pane.
- `agent.prompt` submits work; `agent.wait` can observe `working`, `idle`, `blocked`, or `done`; `agent.read` retrieves terminal output.
- Interrupt an active agent with `agent.send_keys` using `ctrl+c`, then observe the agent state. This is terminal-level cancellation and is best-effort; do not treat the key submission alone as proof that the agent stopped. Ignore late results after the workflow has entered Error.
- Herdr controls the current Herdr-managed session and requires its caller context (`HERDR_ENV=1`). Startup preflight must not create or commandeer a user's panes silently.

CLI help and the installed API schema confirm these command/parameter contracts. A live launch, prompt, output read, and cancellation test is not included here because it would start and interrupt an agent in the current Herdr session; test the adapter with fakes first, then validate live behavior in an explicitly isolated test session.

## Jev

Jev is not installed and is not required by this release. Use the built-in priority/dependency ordering for the MVP. A later change will create a Pi extension that integrates Jev.
