import json
from pathlib import Path
from typing import Any

import pytest

from cronos_ai.pi_rpc import PiRpcSupervisor
from cronos_ai.profiles import FactoryProfileRegistry, ProfileError


def write_registry(root: Path, profile: dict[str, Any]) -> None:
    root.mkdir(parents=True)
    skills = root / "skills" / "python"
    skills.mkdir(parents=True)
    (skills / "SKILL.md").write_text(
        "---\nname: python\n"
        "description: Python implementation guidance.\n"
        "---\nUse pytest.\n"
    )
    (root / "profiles.json").write_text(
        json.dumps(
            {
                "profiles": {profile["name"]: profile},
                "mcp_servers": [
                    {
                        "server_id": "repo",
                        "command": "factory-mcp-repo",
                        "arguments": ["--stdio"],
                        "tools": ["status", "delete"],
                    }
                ],
            }
        )
    )


def make_profile(**overrides: Any) -> dict[str, Any]:
    return {
        "name": "implementer",
        "role": "Implementation",
        "model": "configured-model",
        "provider": "configured-provider",
        "provider_api_key_env": "FACTORY_PROVIDER_KEY",
        "skills": ["python"],
        "mcp_tools": ["repo/status"],
        "network_allowlist": ["https://api.provider.example"],
        **overrides,
    }


def test_registry_resolves_only_explicit_factory_managed_resources(
    tmp_path: Path,
) -> None:
    factory_root = tmp_path / "factory-resources"
    write_registry(factory_root, make_profile())
    project_root = tmp_path / "target-project"
    project_root.mkdir()
    project_skill = project_root / ".agents" / "skills" / "unlisted"
    project_skill.mkdir(parents=True)
    (project_skill / "SKILL.md").write_text("Untrusted project skill\n")
    (project_root / ".pi").mkdir()
    (project_root / ".pi" / "extensions").mkdir()
    (project_root / ".pi" / "extensions" / "mcp.ts").write_text(
        "throw new Error('must not load')\n"
    )

    resolved = FactoryProfileRegistry(factory_root).resolve(
        "implementer",
        target_repository=project_root,
    )

    assert resolved.profile.model == "configured-model"
    assert resolved.profile.provider == "configured-provider"
    assert resolved.profile.provider_api_key_env == "FACTORY_PROVIDER_KEY"
    assert resolved.skill_paths == (factory_root / "skills/python",)
    assert [server.definition.server_id for server in resolved.mcp_servers] == ["repo"]
    assert resolved.mcp_servers[0].tools == ("status",)
    assert project_skill not in resolved.skill_paths
    assert all(project_root not in path.parents for path in resolved.skill_paths)


def test_pi_command_loads_only_registry_resolved_skills_and_mcp_tools(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    factory_root = tmp_path / "factory-resources"
    write_registry(factory_root, make_profile())
    project_root = tmp_path / "target-project"
    unlisted = project_root / ".agents" / "skills" / "unlisted"
    unlisted.mkdir(parents=True)
    (unlisted / "SKILL.md").write_text("Unlisted project skill\n")
    resolved = FactoryProfileRegistry(factory_root).resolve(
        "implementer",
        target_repository=project_root,
    )
    monkeypatch.setenv("FACTORY_PROVIDER_KEY", "profile-test-key")
    supervisor = PiRpcSupervisor(
        resolved.profile,
        project_root,
        resources=resolved,
    )

    command = supervisor.command
    tool_args = command[command.index("--tools") + 1]

    assert command.count("--skill") == 1
    assert "--no-skills" in command
    assert "--no-extensions" in command
    assert "--no-context-files" in command
    assert "--no-approve" in command
    assert str(factory_root / "skills/python") in command
    assert "mcp_repo_status" in tool_args
    assert "mcp_repo_delete" not in tool_args
    extension_index = command.index("--extension")
    extension_path = Path(command[extension_index + 1])
    assert extension_path.is_file()
    child_environment = supervisor.child_environment()
    bridge_config = json.loads(
        Path(child_environment["FACTORY_MCP_CONFIG_PATH"]).read_text()
    )
    assert bridge_config["servers"][0]["tools"] == [
        {"name": "status", "pi_tool_name": "mcp_repo_status"}
    ]
    assert str(unlisted) not in " ".join(command)
    supervisor.close()


def test_registry_rejects_a_symlinked_skills_root(tmp_path: Path) -> None:
    factory_root = tmp_path / "factory-resources"
    factory_root.mkdir()
    outside_skills = tmp_path / "external-skills"
    outside_skill = outside_skills / "python"
    outside_skill.mkdir(parents=True)
    (outside_skill / "SKILL.md").write_text("External skill\n")
    (factory_root / "skills").symlink_to(outside_skills, target_is_directory=True)
    profile = make_profile()
    (factory_root / "profiles.json").write_text(
        json.dumps({"profiles": {"implementer": profile}, "mcp_servers": []})
    )
    target = tmp_path / "target-project"
    target.mkdir()

    with pytest.raises(ProfileError, match="skills root must not be a symlink"):
        FactoryProfileRegistry(factory_root).resolve(
            "implementer",
            target_repository=target,
        )


def test_registry_rejects_profile_root_inside_target_project(tmp_path: Path) -> None:
    project_root = tmp_path / "project"
    factory_root = project_root / ".factory-resources"
    write_registry(factory_root, make_profile())

    with pytest.raises(ProfileError, match="separate from the target project"):
        FactoryProfileRegistry(factory_root).resolve(
            "implementer",
            target_repository=project_root,
        )


def test_registry_rejects_missing_skills_without_falling_back_to_project_resources(
    tmp_path: Path,
) -> None:
    factory_root = tmp_path / "factory-resources"
    write_registry(factory_root, make_profile(skills=["python", "unlisted"]))
    project_skill = tmp_path / "target" / ".agents" / "skills" / "unlisted"
    project_skill.mkdir(parents=True)
    (project_skill / "SKILL.md").write_text("Project fallback\n")

    with pytest.raises(ProfileError, match="factory-managed skill is missing"):
        FactoryProfileRegistry(factory_root).resolve(
            "implementer",
            target_repository=tmp_path / "target",
        )


def test_registry_rejects_mcp_tools_not_declared_by_factory_server(
    tmp_path: Path,
) -> None:
    factory_root = tmp_path / "factory-resources"
    write_registry(factory_root, make_profile(mcp_tools=["repo/delete"]))
    registry_file = factory_root / "profiles.json"
    config = json.loads(registry_file.read_text())
    config["mcp_servers"][0]["tools"] = [
        tool
        for tool in config["mcp_servers"][0]["tools"]
        if tool == "status"
    ]
    registry_file.write_text(json.dumps(config))
    target_repository = tmp_path / "target-project"
    target_repository.mkdir()

    with pytest.raises(ProfileError, match="not declared by factory configuration"):
        FactoryProfileRegistry(factory_root).resolve(
            "implementer",
            target_repository=target_repository,
        )


def test_registry_rejects_a_skill_symlink_outside_factory_root(tmp_path: Path) -> None:
    factory_root = tmp_path / "factory-resources"
    write_registry(factory_root, make_profile())
    outside = tmp_path / "outside-skill"
    outside.mkdir()
    (outside / "SKILL.md").write_text("External resource\n")
    python_skill = factory_root / "skills" / "python"
    python_skill.rename(factory_root / "skills" / "python-original")
    python_skill.symlink_to(outside, target_is_directory=True)

    target_repository = tmp_path / "target-project"
    target_repository.mkdir()
    with pytest.raises(ProfileError, match="must not be a symlink"):
        FactoryProfileRegistry(factory_root).resolve(
            "implementer",
            target_repository=target_repository,
        )
