import json
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

from cronos_ai.mcp_bridge import prepare_mcp_bridge
from cronos_ai.profiles import FactoryProfileRegistry

FAKE_MCP_SERVER = """import json
import os
import sys

for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    if method == "initialize":
        if request["params"]["clientInfo"]["name"] != "cronos-ai":
            raise SystemExit("unexpected MCP client identity")
        result = {
            "protocolVersion": request["params"]["protocolVersion"],
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "test-server", "version": "1"},
        }
    elif method == "tools/list":
        result = {
            "tools": [
                {
                    "name": "status",
                    "description": "Untrusted description must not reach the model.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "path": {
                                "type": "string",
                                "description": "file path",
                            }
                        },
                        "required": ["path"],
                        "additionalProperties": False,
                    },
                },
                {
                    "name": "delete",
                    "description": "Unlisted destructive tool.",
                    "inputSchema": {"type": "object", "properties": {}},
                },
            ]
        }
    elif method == "tools/call":
        result = {
            "content": [
                {
                    "type": "text",
                    "text": (
                        "provider-key-forwarded"
                        if os.environ.get("FACTORY_PROVIDER_KEY")
                        else "provider-key-not-forwarded"
                    ),
                }
            ]
        }
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


def test_bridge_invokes_only_factory_allowlisted_tools_and_strips_provider_key(
    tmp_path: Path,
) -> None:
    if shutil.which("node") is None:
        pytest.skip("Node.js unavailable")
    factory_root = tmp_path / "factory-resources"
    factory_root.mkdir()
    fake_server = factory_root / "fake_mcp_server.py"
    fake_server.write_text(FAKE_MCP_SERVER)
    target = tmp_path / "target-project"
    target.mkdir()
    (factory_root / "profiles.json").write_text(
        json.dumps(
            {
                "profiles": {
                    "implementer": {
                        "name": "implementer",
                        "role": "Implementation",
                        "model": "configured-model",
                        "provider": "configured-provider",
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
                        "arguments": [str(fake_server)],
                        "tools": ["status", "delete"],
                    }
                ],
            }
        )
    )
    resolved = FactoryProfileRegistry(factory_root).resolve(
        "implementer",
        target_repository=target,
    )
    runtime = tmp_path / "runtime"
    bridge = prepare_mcp_bridge(resolved, runtime)
    package = runtime / "node_modules" / "typebox"
    package.mkdir(parents=True)
    (package / "package.json").write_text(
        json.dumps(
            {
                "name": "typebox",
                "version": "1.0.0",
                "type": "module",
                "exports": "./index.js",
            }
        )
    )
    (package / "index.js").write_text(
        "export const Type = { Unsafe: (schema) => schema };\n"
    )
    harness = runtime / "harness.mjs"
    harness.write_text(
        """import { pathToFileURL } from 'node:url';
const module = await import(pathToFileURL(process.argv[2]).href);
const tools = new Map();
const handlers = new Map();
const statusUpdates = [];
const pi = {
  registerTool(tool) { tools.set(tool.name, tool); },
  on(name, handler) { handlers.set(name, handler); },
};
await module.default(pi);
await handlers.get('session_start')({}, {
  ui: { setStatus: (key, text) => statusUpdates.push({ key, text }) },
});
const tool = tools.get('mcp_repo_status');
const result = await tool.execute('call-1', { path: '/workspace/README.md' });
await handlers.get('session_shutdown')();
console.log(JSON.stringify({ names: [...tools.keys()], result, statusUpdates }));
"""
    )
    environment = {
        "PATH": str(Path(shutil.which("node") or "node").parent),
        "FACTORY_MCP_CONFIG_PATH": str(bridge.config_path),
        "FACTORY_PROVIDER_KEY": "host-provider-secret",
        "HOME": str(tmp_path),
        "TMPDIR": "/tmp",
    }

    execution = subprocess.run(
        [
            shutil.which("node") or "node",
            "--experimental-strip-types",
            str(harness),
            str(bridge.extension_path),
        ],
        cwd=runtime,
        env=environment,
        capture_output=True,
        text=True,
        check=False,
        timeout=20,
    )

    assert execution.returncode == 0, execution.stderr
    output = json.loads(execution.stdout.strip().splitlines()[-1])
    assert output["names"] == ["mcp_repo_status"]
    assert output["result"]["content"] == [
        {"type": "text", "text": "provider-key-not-forwarded"}
    ]
    assert output["statusUpdates"][-1] == {
        "key": "factory-mcp",
        "text": json.dumps(
            {"state": "ready", "tools": ["mcp_repo_status"]},
            separators=(",", ":"),
        ),
    }
