# Proposal

## Why

This repository currently describes an AI software factory but has only a Python package scaffold, so it cannot accept work, coordinate agents, or deliver verified changes. Implementing the factory as a local-first orchestrator will turn the documented workflow into a recoverable, human-supervised system for planning, executing, reviewing, and feeding production alerts back into work intake.

## What Changes

- Add a local CLI with an explicit `factory init --repo <path>` setup command and a run command that accepts work only for an initialized, clean Git repository and creates an OpenSpec plan for every accepted request.
- Add a Python + LangGraph orchestrator with durable SQLite execution state, dependency-aware scheduling, and recovery of active Herdr workers after restart.
- Run specialist Pi agents in reusable Herdr worker slots, using curated profiles with explicitly managed skills and allowlisted MCP tools.
- Keep generated plans and task results on a run branch, isolate concurrent implementation tasks in separate Git worktrees and Docker-compatible containers, merge clean task results automatically, and send conflicts to human attention.
- Add fine-grained task states, bounded retries for transient failures, a human attention queue, and approval gates for substantial, ambiguous, and high-impact work.
- Integrate the human-supervised workflow with the target repository's existing CI/CD pipeline rather than providing a generic deployment system.
- Accept authenticated, deduplicated, provider-neutral monitoring webhooks through a user-configured reachable endpoint and send resulting tasks through normal triage.
- Report worker state and task summaries in Herdr; provide factory CLI actions for approvals, retries, and conflict resolution.

## Capabilities

### New Capabilities
- `factory-orchestration`: Accept and plan work, track durable execution state, schedule dependency-ready tasks, and recover runs.
- `agent-execution`: Run curated Pi specialists through Herdr with isolated worktrees, sandboxing, bounded retries, and safe integration.
- `human-oversight`: Present actionable work and enforce human approval gates across planning, review, and integration.
- `factory-integrations`: Receive production alerts and invoke existing CI/CD workflows without owning deployment infrastructure.

### Modified Capabilities

None.

## Impact

The Python package and CLI, LangGraph orchestration, SQLite persistence, OpenSpec artifact generation, Git worktree management, Herdr CLI/Socket API integration, Pi RPC processes, Docker-compatible sandbox runtime, MCP and skill configuration, webhook intake, factory CLI approvals, and target-repository CI/CD adapters. The repository currently has no capability specs or implemented integrations to preserve.
