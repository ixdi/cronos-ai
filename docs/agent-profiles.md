# Configure workflow-agent profiles

Cronos-AI profiles let different workflow agents load only the Pi skills, extensions, and MCP servers appropriate to their role. Rust validates each selected profile against the active workflow step before any worker starts.

## Manage profiles in the TUI

On the project list, press **p** to open **Agent profiles**. The table lists every profile with its allowed steps, number of skills and extensions, MCP, model, time limit, and whether it is ready. The panel below shows the selected profile's resource paths, marking each one ✓ (exists) or ✕ (missing).

| Key | Action |
|---|---|
| **n** | Create a profile |
| **Enter** or **e** | Edit the selected profile |
| **d** | Delete the selected profile (confirm with **y**) |
| **r** | Reload the catalog from disk |
| **Backspace**/**Esc** | Back to projects |

The editor has fields for the ID, description, steps, model, time limit, skills, extensions, MCP adapter, and MCP config, with help for the focused field on the right:

- **Tab**/**Shift+Tab** move between fields; in text fields **↑/↓** do too. **Enter** saves and **Esc** discards.
- **Steps:** **↑/↓** choose a step and **Space** toggles it.
- **Skills** and **Extensions:** type a path and press **Enter** to add it; with nothing typed, **Enter** saves. **↑/↓** select a path and **Delete** removes it. A leading `~/` expands to your home directory.
- **MCP adapter** and **MCP config** must both be set or both empty.

Saving checks the profile completely, including that every path exists, and keeps the editor open with the error if something is wrong. The catalog is written atomically, so a step starting at the same moment reads the old or the new version. If the catalog already contains a broken profile, for example because a skill was moved, the TUI still opens it, marks the profile as a problem, and lets you fix or delete it; other profiles can be edited meanwhile. Until it is fixed, workflow steps cannot load the catalog.

## Create the profile catalog by hand

Cronos-AI reads `agent-profiles.json` from its data directory:

- `CRONOS_AI_DATA_DIR`, when set;
- otherwise, `$XDG_DATA_HOME/cronos-ai`, or `~/.local/share/cronos-ai` when `XDG_DATA_HOME` is unset;
- on Windows, `%APPDATA%/cronos-ai`.

You can also write the file yourself. Copy the example catalog, then edit the copy:

```sh
profile_dir="${CRONOS_AI_DATA_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/cronos-ai}"
mkdir -p "$profile_dir"
cp examples/agent-profiles.json "$profile_dir/agent-profiles.json"
```

If Cronos-AI was started with a custom `--data-dir`, put the file in that directory instead. A missing catalog means there are no profiles; Cronos-AI does not create or install user-configured resources automatically.

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

Every worker also loads Cronos-AI's built-in skill for its step, before any profile skills:

| Step | Built-in skill |
|---|---|
| `specification-generation` | `cronos-specification-generation`: write a reviewable, testable OpenSpec change |
| `code-generation` | `cronos-code-generation`: implement test-first in small commits and leave the worktree clean |
| `code-review` | `cronos-code-review`: review read-only against the change and report verified findings |
| `verification` | `cronos-verification`: run the project's tests, linters, and audits read-only |
| `documentation` | `cronos-documentation`: update only the assigned docs, without committing |

The skills are embedded in the binary and written to `<data-dir>/skills/<name>/SKILL.md` when a step starts; an edited copy is restored, so put your own guidance in a profile skill instead.
Their sources are in [`skills/`](../skills/).

If an agent has no `profile_id`, it gets its step's built-in skill and no optional user-configured skills, extensions, or MCP servers. Editing the catalog affects future worker launches; already-running agents keep their current resources.

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
