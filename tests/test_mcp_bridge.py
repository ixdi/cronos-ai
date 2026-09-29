import json
from pathlib import Path
from typing import Any

from cronos_ai.mcp_bridge import prepare_mcp_bridge
from cronos_ai.profiles import FactoryProfileRegistry, pi_mcp_tool_name


def create_resolved_profile(root: Path):
    skills = root / "skills" / "python"
    skills.mkdir(parents=True)
    (skills / "SKILL.md").write_text(
        "---\nname: python\ndescription: Python skill.\n---\n"
    )
    profile: dict[str, Any] = {
        "name": "implementer",
        "role": "Implementation",
        "model": "configured-model",
        "provider": "configured-provider",
        "provider_api_key_env": "FACTORY_PROVIDER_KEY",
        "skills": ["python"],
        "mcp_tools": ["repo/status"],
        "network_allowlist": ["https://api.provider.example"],
    }
    (root / "profiles.json").write_text(
        json.dumps(
            {
                "profiles": {"implementer": profile},
                "mcp_servers": [
                    {
                        "server_id": "repo",
                        "command": "factory-mcp-repo",
                        "arguments": ["--stdio"],
                        "tools": ["status", "delete"],
                    },
                    {
                        "server_id": "unlisted",
                        "command": "unlisted-server",
                        "arguments": [],
                        "tools": ["admin"],
                    },
                ],
            }
        )
    )
    target_repository = root.parent / "target-project"
    target_repository.mkdir()
    return FactoryProfileRegistry(root).resolve(
        "implementer",
        target_repository=target_repository,
    )


def test_bridge_config_contains_only_profile_selected_servers_and_tools(
    tmp_path: Path,
) -> None:
    resolved = create_resolved_profile(tmp_path / "factory-resources")

    files = prepare_mcp_bridge(resolved, tmp_path / "runtime")
    config = json.loads(files.config_path.read_text())
    source = files.extension_path.read_text()

    assert [server["server_id"] for server in config["servers"]] == ["repo"]
    assert config["servers"][0]["tools"] == [
        {"name": "status", "pi_tool_name": "mcp_repo_status"}
    ]
    assert "delete" not in files.config_path.read_text()
    assert "unlisted-server" not in files.config_path.read_text()
    assert files.tool_names == ("mcp_repo_status",)
    assert "tools/list" in source
    assert "tools/call" in source
    assert "FACTORY_MCP_CONFIG_PATH" in source
    assert "process.env" not in source or "...process.env" not in source
    assert files.extension_path.parent == tmp_path / "runtime"


def test_pi_mcp_tool_names_are_stable_safe_and_bounded() -> None:
    assert pi_mcp_tool_name("repo", "status") == "mcp_repo_status"
    name = pi_mcp_tool_name("x" * 32, "y" * 64)

    assert name.startswith("mcp_")
    assert len(name) <= 64
    assert name.replace("_", "").isalnum()
