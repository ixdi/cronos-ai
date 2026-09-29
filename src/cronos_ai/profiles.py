"""Factory-managed specialist profile and resource resolution."""

from __future__ import annotations

import hashlib
import json
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from pydantic import ValidationError

from cronos_ai.models import (
    MCPServerDefinition,
    SpecialistProfile,
    ValidatedModel,
)


def pi_mcp_tool_name(server_id: str, tool_name: str) -> str:
    """Create a stable Pi-compatible name for one selected MCP tool."""
    raw_name = f"mcp_{server_id}_{tool_name}"
    safe_name = re.sub(r"[^A-Za-z0-9_-]", "_", raw_name)
    if len(safe_name) <= 64:
        return safe_name
    digest = hashlib.sha256(raw_name.encode("utf-8")).hexdigest()[:8]
    return f"{safe_name[:55]}_{digest}"


class ProfileError(ValueError):
    """Raised when a specialist profile references unapproved resources."""


class _ProfileManifest(ValidatedModel):
    profiles: dict[str, SpecialistProfile]
    mcp_servers: tuple[MCPServerDefinition, ...] = ()


@dataclass(frozen=True)
class ResolvedMCPServer:
    """A factory server definition narrowed to profile-selected tools."""

    definition: MCPServerDefinition
    tools: tuple[str, ...]


@dataclass(frozen=True)
class ResolvedSpecialistProfile:
    """A profile resolved only against explicitly configured factory resources."""

    resource_root: Path
    profile: SpecialistProfile
    skill_paths: tuple[Path, ...]
    mcp_servers: tuple[ResolvedMCPServer, ...]


class FactoryProfileRegistry:
    """Load profiles from one explicit, factory-managed resource root."""

    def __init__(self, root: Path) -> None:
        if root.is_symlink():
            raise ProfileError("factory profile root must not be a symlink")
        try:
            self.root = root.resolve(strict=True)
        except OSError as error:
            raise ProfileError("factory profile root does not exist") from error
        if not self.root.is_dir():
            raise ProfileError("factory profile root must be a directory")
        self.manifest_path = self.root / "profiles.json"

    def _load_manifest(self) -> _ProfileManifest:
        if self.manifest_path.is_symlink() or not self.manifest_path.is_file():
            raise ProfileError("factory profile manifest is missing or unsafe")
        try:
            document = json.loads(self.manifest_path.read_text(encoding="utf-8"))
            return _ProfileManifest.model_validate(document)
        except (OSError, json.JSONDecodeError, ValidationError) as error:
            raise ProfileError("factory profile manifest is invalid") from error

    def resolve(
        self,
        profile_name: str,
        *,
        target_repository: Path,
    ) -> ResolvedSpecialistProfile:
        """Resolve listed resources from the factory root, never the target tree."""
        try:
            target = target_repository.resolve(strict=True)
        except OSError as error:
            raise ProfileError("target repository does not exist") from error
        if self.root.is_relative_to(target) or target.is_relative_to(self.root):
            raise ProfileError(
                "factory profile root must be separate from the target project"
            )

        manifest = self._load_manifest()
        profile = manifest.profiles.get(profile_name)
        if profile is None:
            raise ProfileError(
                f"factory specialist profile is not configured: {profile_name}"
            )
        if profile.name != profile_name:
            raise ProfileError("profile name must match its registry key")

        skill_paths = tuple(
            self._resolve_skill(skill_name) for skill_name in profile.skills
        )
        server_by_id: dict[str, MCPServerDefinition] = {}
        for server in manifest.mcp_servers:
            if server.server_id in server_by_id:
                raise ProfileError(f"duplicate MCP server id: {server.server_id}")
            server_by_id[server.server_id] = server

        selected_tools: dict[str, list[str]] = {}
        for reference in profile.mcp_tools:
            server_id, separator, tool_name = reference.partition("/")
            if not separator or not server_id or not tool_name:
                raise ProfileError(f"invalid MCP tool reference: {reference}")
            configured_server = server_by_id.get(server_id)
            if configured_server is None:
                raise ProfileError(
                    "MCP server is not declared by factory configuration: "
                    f"{server_id}"
                )
            if tool_name not in configured_server.tools:
                raise ProfileError(
                    f"MCP tool is not declared by factory configuration: {reference}"
                )
            selected_tools.setdefault(server_id, []).append(tool_name)

        selected_servers = tuple(
            ResolvedMCPServer(
                definition=server_by_id[server_id],
                tools=tuple(tools),
            )
            for server_id, tools in selected_tools.items()
        )
        return ResolvedSpecialistProfile(
            resource_root=self.root,
            profile=profile,
            skill_paths=skill_paths,
            mcp_servers=selected_servers,
        )

    def _resolve_skill(self, skill_name: str) -> Path:
        skills_root = self.root / "skills"
        if skills_root.is_symlink():
            raise ProfileError("factory-managed skills root must not be a symlink")
        skill_path = skills_root / skill_name
        if skill_path.is_symlink():
            raise ProfileError(
                f"factory-managed skill must not be a symlink: {skill_name}"
            )
        try:
            resolved_root = skills_root.resolve(strict=True)
            resolved_skill = skill_path.resolve(strict=True)
        except OSError as error:
            raise ProfileError(
                f"factory-managed skill is missing: {skill_name}"
            ) from error
        if not resolved_skill.is_relative_to(resolved_root):
            raise ProfileError(
                f"factory-managed skill escapes its resource root: {skill_name}"
            )
        if not resolved_skill.is_dir() or not (resolved_skill / "SKILL.md").is_file():
            raise ProfileError(
                f"factory-managed skill is missing: {skill_name}"
            )
        for resource in resolved_skill.rglob("*"):
            if (
                resource.is_symlink()
                or not resource.resolve().is_relative_to(resolved_skill)
            ):
                raise ProfileError(
                    f"factory-managed skill contains an unsafe path: {skill_name}"
                )
        return resolved_skill


def profile_resource_manifest(
    resolved: ResolvedSpecialistProfile,
) -> dict[str, Any]:
    """Serialize only the selected profile resources for the Pi MCP bridge."""
    return {
        "servers": [
            {
                "server_id": server.definition.server_id,
                "command": server.definition.command,
                "arguments": list(server.definition.arguments),
                "tools": [
                    {
                        "name": tool_name,
                        "pi_tool_name": pi_mcp_tool_name(
                            server.definition.server_id,
                            tool_name,
                        ),
                    }
                    for tool_name in server.tools
                ],
            }
            for server in resolved.mcp_servers
        ]
    }
