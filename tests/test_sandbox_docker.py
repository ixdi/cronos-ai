import os
import shutil
import subprocess
from pathlib import Path

import pytest

from cronos_ai.models import SpecialistProfile
from cronos_ai.sandbox import DockerSandboxAdapter


@pytest.mark.skipif(
    os.environ.get("CRONOS_AI_DOCKER_E2E") != "1"
    or shutil.which("docker") is None,
    reason="set CRONOS_AI_DOCKER_E2E=1 with a running Docker daemon",
)
def test_live_docker_sandbox_mounts_only_worktree_and_denies_unlisted_egress(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    worktree = tmp_path / "task-worktree"
    worktree.mkdir()
    subprocess.run(
        ["git", "init", "--quiet", str(worktree)],
        check=True,
        capture_output=True,
        text=True,
    )
    (worktree / "marker.txt").write_text("mounted\n")
    api_key = "docker-e2e-test-key"
    monkeypatch.setenv("FACTORY_E2E_PROVIDER_KEY", api_key)
    profile = SpecialistProfile(
        name="docker-e2e",
        role="Sandbox verification",
        model="test-model",
        provider="test-provider",
        provider_api_key_env="FACTORY_E2E_PROVIDER_KEY",
        network_allowlist=(),
    )
    command = """
import os
import pathlib
import socket
import urllib.error
import urllib.request

assert pathlib.Path('/workspace/marker.txt').read_text() == 'mounted' + chr(10)
pathlib.Path('/workspace/result.txt').write_text('task-write-ok')
assert os.environ.get('FACTORY_E2E_PROVIDER_KEY') == 'docker-e2e-test-key'
try:
    pathlib.Path('/root/.ssh').stat()
    host_auth_is_accessible = True
except OSError:
    host_auth_is_accessible = False
assert not host_auth_is_accessible
sock = socket.socket()
sock.settimeout(2)
direct = sock.connect_ex(('1.1.1.1', 443))
sock.close()
assert direct != 0, 'direct egress unexpectedly succeeded'
try:
    urllib.request.urlopen('https://example.com', timeout=5)
except urllib.error.URLError as error:
    assert '403' in str(error), str(error)
else:
    raise AssertionError('unlisted proxy destination unexpectedly succeeded')
"""

    result = DockerSandboxAdapter("python:3.12-alpine3.22").run(
        worktree,
        profile,
        ("python3", "-c", command),
    )

    assert result.returncode == 0, result.stderr
    assert (worktree / "result.txt").read_text() == "task-write-ok"
