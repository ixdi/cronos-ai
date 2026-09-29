import json
import shutil
import sys
from pathlib import Path

import pytest

from cronos_ai.pi_rpc import PiRpcSupervisor
from cronos_ai.profiles import FactoryProfileRegistry

FAKE_MCP_SERVER = """import json
import os
import pathlib
import sys

pathlib.Path(sys.argv[1]).write_text(
    'present' if os.environ.get('FACTORY_PROVIDER_KEY') else 'absent'
)
for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    if method == "initialize":
        result = {
            "protocolVersion": request["params"]["protocolVersion"],
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "factory-test", "version": "1"},
        }
    elif method == "tools/list":
        result = {
            "tools": [
                {
                    "name": "status",
                    "description": "Read repository status.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {},
                        "additionalProperties": False,
                    },
                },
                {
                    "name": "delete",
                    "description": "Delete a file.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {"path": {"type": "string"}},
                        "required": ["path"],
                    },
                },
            ]
        }
    elif method == "tools/call":
        result = {"content": [{"type": "text", "text": "called"}]}
    else:
        if "id" in request:
            print(
                json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": {}}),
                flush=True,
            )
        continue
    if "id" in request:
        print(
            json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}),
            flush=True,
        )
"""


@pytest.mark.skipif(shutil.which("pi") is None, reason="Pi CLI unavailable")
def test_pi_rpc_loads_only_profile_allowlisted_mcp_tools(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    factory_root = tmp_path / "factory-resources"
    factory_root.mkdir()
    fake_server = factory_root / "fake_mcp_server.py"
    fake_server.write_text(FAKE_MCP_SERVER)
    mcp_environment_marker = tmp_path / "mcp-environment.txt"
    (factory_root / "profiles.json").write_text(
        json.dumps(
            {
                "profiles": {
                    "implementer": {
                        "name": "implementer",
                        "role": "Implementation",
                        "model": "gpt-4o-mini",
                        "provider": "openai",
                        "provider_api_key_env": "FACTORY_PROVIDER_KEY",
                        "skills": [],
                        "mcp_tools": ["repo/status"],
                        "network_allowlist": [],
                    }
                },
                "mcp_servers": [
                    {
                        "server_id": "repo",
                        "command": sys.executable,
                        "arguments": [str(fake_server), str(mcp_environment_marker)],
                        "tools": ["status", "delete"],
                    }
                ],
            }
        )
    )
    target_worktree = tmp_path / "target-worktree"
    target_worktree.mkdir()
    resolved = FactoryProfileRegistry(factory_root).resolve(
        "implementer",
        target_repository=target_worktree,
    )
    monkeypatch.setenv("FACTORY_PROVIDER_KEY", "profile-test-key")
    monkeypatch.setenv("PI_OFFLINE", "1")
    supervisor = PiRpcSupervisor(
        resolved.profile,
        target_worktree,
        resources=resolved,
        executable=shutil.which("pi") or "pi",
        mcp_startup_timeout=20,
    )

    try:
        process = supervisor.start()
        assert process.poll() is None
        assert supervisor.active_mcp_tools == ("mcp_repo_status",)
        assert mcp_environment_marker.read_text() == "absent"
    finally:
        supervisor.close()
