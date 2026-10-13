# Configure workflow-agent profiles

Cronos-AI profiles let different workflow agents load only the Pi skills, extensions, and MCP servers appropriate to their role. Rust validates each selected profile against the active workflow step before any worker starts.

## Create the profile catalog

Cronos-AI reads `agent-profiles.json` from its data directory:

- `CRONOS_AI_DATA_DIR`, when set;
- otherwise, `$XDG_DATA_HOME/cronos-ai`, or `~/.local/share/cronos-ai` when `XDG_DATA_HOME` is unset;
- on Windows, `%APPDATA%/cronos-ai`.

Copy the example catalog, then edit the copy:

```sh
profile_dir="${CRONOS_AI_DATA_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/cronos-ai}"
mkdir -p "$profile_dir"
cp examples/agent-profiles.json "$profile_dir/agent-profiles.json"
```

If Cronos-AI was started with a custom `--data-dir`, put the file in that directory instead. A missing catalog means there are no profiles; Cronos-AI does not create or install resources automatically.

A profile has a stable `id`, a description, allowed workflow `steps`, and optional resource lists. IDs start with a lowercase letter and contain only lowercase letters, digits, `_`, or `-` (up to 64 characters). Descriptions are nonblank and at most 500 characters. Each profile must list at least one agent-running step:

- `specification-generation`
- `code-generation`
- `code-review`
- `verification`
- `documentation`

Use absolute paths to existing skill files/directories and Pi extension files/directories. Each list is limited to 32 paths. Example resource fields:

```json
"skills": ["/home/alice/.agents/skills/security-and-hardening"],
"extensions": ["/home/alice/.pi/agent/extensions/review-tools.ts"]
```

Two optional fields tune the worker itself:

- `model`: passed to Pi as `--model`. It starts with a letter or digit and contains only letters, digits, and `. _ : / -`, for example `"anthropic/claude-sonnet-5-5"`.
- `timeout_minutes`: the agent's time limit, 1 to 480 minutes, replacing the step default (30 minutes for specification generation, code review, and documentation; 60 for code generation and verification). An agent that exceeds it is cancelled and fails the step.

```json
"model": "provider/review-model",
"timeout_minutes": 15
```

The complete, schema-valid catalog is in [`examples/agent-profiles.json`](../examples/agent-profiles.json). Copy it and add actual resource paths; an empty profile is valid but adds no optional resources.

## Assign a profile to an agent

During a workflow step, the Pi orchestrator can call `cronos_agent_profiles` to see profiles allowed for that step. It may select a profile by adding its `id` as `profile_id` to that agent's plan. Different agents in the same step can use different profiles. Rust rejects unknown IDs or profiles assigned to another step before launching any sibling agent.

If an agent has no `profile_id`, it gets no optional user-configured skills, extensions, or MCP servers. Editing the catalog affects future worker launches; already-running agents keep their current resources.

## Configure MCP servers

MCP is optional and needs a Pi MCP adapter that supports the `--mcp-config` option. For example, `pi-mcp-adapter` can be installed explicitly by the user:

```sh
pi install npm:pi-mcp-adapter
```

Cronos-AI never installs it automatically. When a profile needs MCP, add an `mcp` object containing both the absolute adapter-extension path and a separate Pi-compatible JSON config path:

```json
"mcp": {
  "adapter": "/absolute/path/to/pi-mcp-adapter",
  "config": "/absolute/path/to/security-review-mcp.json"
}
```

The MCP file must contain a nonempty `mcpServers` object. [`examples/security-review-mcp.json`](../examples/security-review-mcp.json) is a schema-valid template; replace its example endpoint with a server you use.

Keep credentials out of the profile catalog and source control. Put the real MCP config in a user-owned file with appropriately protected permissions and use environment-variable references for secrets supported by the adapter. Include only servers intended for that profile. Cronos-AI explicitly selects that file for the worker; it does not fall back to unrelated global or project MCP configurations. Missing adapters, malformed configs, or unavailable resource paths stop the affected step with an error.

## Trust and scripts

Pi extensions execute with the operating-system permissions of the Cronos-AI user; review them before assigning them. Cronos-AI does not run arbitrary shell lifecycle hooks from profiles. If an agent needs supporting scripts, bundle them with a skill and describe when to use them in `SKILL.md`, or implement the behavior in a reviewed Pi extension. The profile ID makes that assignment explicit.
