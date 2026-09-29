import subprocess
from collections.abc import Sequence
from pathlib import Path

import pytest

from cronos_ai.models import SpecialistProfile
from cronos_ai.sandbox import (
    DockerSandboxAdapter,
    SandboxError,
)

API_KEY = "sandbox-test-secret"


class FakeDocker:
    def __init__(
        self,
        *,
        image_cached: bool = True,
        fail_gateway: bool = False,
        fail_build: bool = False,
    ) -> None:
        self.calls: list[tuple[list[str], dict[str, object]]] = []
        self.image_cached = image_cached
        self.fail_gateway = fail_gateway
        self.fail_build = fail_build
        self.build_files: set[str] = set()

    def __call__(
        self,
        command: Sequence[str],
        **kwargs: object,
    ) -> subprocess.CompletedProcess[str]:
        args = list(command)
        self.calls.append((args, kwargs))
        if args[1:3] == ["run", "--detach"]:
            code = 1 if self.fail_gateway else 0
            return subprocess.CompletedProcess(args, code, "gateway-container-id\n", "")
        if args[1:3] == ["run", "--rm"]:
            return subprocess.CompletedProcess(args, 7, API_KEY, "worker failure")
        if args[1:3] == ["image", "inspect"]:
            code = 0 if self.image_cached else 1
            return subprocess.CompletedProcess(args, code, "image-id", "")
        if args[1] == "build":
            context = Path(kwargs["cwd"])
            self.build_files = {path.name for path in context.iterdir()}
            code = 1 if self.fail_build else 0
            self.image_cached = not self.fail_build
            return subprocess.CompletedProcess(args, code, "", "")
        return subprocess.CompletedProcess(args, 0, "", "")


def create_worktree(path: Path) -> Path:
    path.mkdir()
    subprocess.run(
        ["git", "init", "--quiet", str(path)],
        check=True,
        capture_output=True,
        text=True,
    )
    return path.resolve()


def make_profile() -> SpecialistProfile:
    return SpecialistProfile(
        name="implementer",
        role="Implementation",
        model="model-a",
        provider="provider-a",
        provider_api_key_env="FACTORY_PROVIDER_KEY",
        network_allowlist=("https://api.provider.example",),
    )


def test_worker_has_one_worktree_mount_and_only_the_configured_key(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    worktree = create_worktree(tmp_path / "task-worktree")
    fake_docker = FakeDocker()
    monkeypatch.setenv("FACTORY_PROVIDER_KEY", API_KEY)

    result = DockerSandboxAdapter(
        "factory-worker:test",
        command_runner=fake_docker,
    ).run(worktree, make_profile(), ("python", "-c", "print('worker')"))

    worker_command = next(
        command
        for command, _ in fake_docker.calls
        if command[1:3] == ["run", "--rm"]
    )
    bind_mounts = [
        arg for arg in worker_command if "type=bind," in arg
    ]
    assert bind_mounts == [
        f"--mount=type=bind,source={worktree},target=/workspace"
    ]
    assert "--network=container:gateway-container-id" in worker_command
    assert "--cap-drop=ALL" in worker_command
    assert "--read-only" in worker_command
    assert "--security-opt=no-new-privileges:true" in worker_command
    worker_uid = worktree.stat().st_uid
    worker_gid = worktree.stat().st_gid
    assert f"--user={worker_uid}:{worker_gid}" in worker_command
    assert "--env=FACTORY_PROVIDER_KEY" in worker_command
    assert f"--env=FACTORY_PROVIDER_KEY={API_KEY}" not in worker_command
    assert API_KEY not in " ".join(worker_command)
    assert "--env=HTTPS_PROXY=http://127.0.0.1:3128" in worker_command
    assert str(Path.home()) not in " ".join(worker_command)
    assert result.returncode == 7
    assert result.stdout == "[REDACTED]"


def test_gateway_has_network_admin_but_worker_does_not(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    worktree = create_worktree(tmp_path / "task-worktree")
    fake_docker = FakeDocker()
    monkeypatch.setenv("FACTORY_PROVIDER_KEY", API_KEY)

    DockerSandboxAdapter(
        "factory-worker:test",
        command_runner=fake_docker,
    ).run(worktree, make_profile(), ("true",))

    gateway_command = next(
        command
        for command, _ in fake_docker.calls
        if command[1:3] == ["run", "--detach"]
    )
    worker_command = next(
        command
        for command, _ in fake_docker.calls
        if command[1:3] == ["run", "--rm"]
    )
    assert "--cap-add=NET_ADMIN" in gateway_command
    assert "--cap-add=SETUID" in gateway_command
    assert "--cap-add=SETGID" in gateway_command
    assert "--cap-drop=ALL" in gateway_command
    assert "--network=bridge" in gateway_command
    assert "--cap-add=NET_ADMIN" not in worker_command
    assert any(
        arg == '--env=FACTORY_EGRESS_ALLOWLIST=["api.provider.example"]'
        for arg in gateway_command
    )
    assert API_KEY not in " ".join(gateway_command)
    assert any(command[1:3] == ["rm", "-f"] for command, _ in fake_docker.calls)


def test_gateway_image_build_contains_only_the_policy_runtime(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    worktree = create_worktree(tmp_path / "task-worktree")
    fake_docker = FakeDocker(image_cached=False)
    monkeypatch.setenv("FACTORY_PROVIDER_KEY", API_KEY)

    DockerSandboxAdapter(
        "factory-worker:test",
        command_runner=fake_docker,
    ).run(worktree, make_profile(), ("true",))

    assert fake_docker.build_files == {
        "Dockerfile",
        "gateway-entrypoint.sh",
        "egress_gateway.py",
    }
    build_command = next(
        command
        for command, _ in fake_docker.calls
        if command[1] == "build"
    )
    assert "--pull" in build_command
    tag_index = build_command.index("--tag")
    assert build_command[tag_index + 1].startswith("cronos-ai-egress:sha256-")


def test_gateway_start_failure_never_launches_worker(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    worktree = create_worktree(tmp_path / "task-worktree")
    fake_docker = FakeDocker(fail_gateway=True)
    monkeypatch.setenv("FACTORY_PROVIDER_KEY", API_KEY)

    with pytest.raises(SandboxError, match="could not start the egress gateway"):
        DockerSandboxAdapter(
            "factory-worker:test",
            command_runner=fake_docker,
        ).run(worktree, make_profile(), ("true",))

    assert not any(
        command[1:3] == ["run", "--rm"]
        for command, _ in fake_docker.calls
    )


def test_gateway_build_failure_never_launches_worker(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    worktree = create_worktree(tmp_path / "task-worktree")
    fake_docker = FakeDocker(image_cached=False, fail_build=True)
    monkeypatch.setenv("FACTORY_PROVIDER_KEY", API_KEY)

    with pytest.raises(SandboxError, match="could not build the required egress"):
        DockerSandboxAdapter(
            "factory-worker:test",
            command_runner=fake_docker,
        ).run(worktree, make_profile(), ("true",))

    assert not any(
        command[1:3] == ["run", "--rm"]
        for command, _ in fake_docker.calls
    )


def test_missing_provider_key_fails_before_any_container_is_started(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    worktree = create_worktree(tmp_path / "task-worktree")
    fake_docker = FakeDocker()
    monkeypatch.delenv("FACTORY_PROVIDER_KEY", raising=False)

    with pytest.raises(SandboxError, match="provider API key"):
        DockerSandboxAdapter(
            "factory-worker:test",
            command_runner=fake_docker,
        ).run(worktree, make_profile(), ("true",))

    assert fake_docker.calls == []


def test_unavailable_docker_daemon_fails_closed_before_worker_launch(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    worktree = create_worktree(tmp_path / "task-worktree")
    calls: list[list[str]] = []

    def unavailable_docker(
        command: Sequence[str],
        **kwargs: object,
    ) -> subprocess.CompletedProcess[str]:
        args = list(command)
        calls.append(args)
        return subprocess.CompletedProcess(args, 1, "", "daemon unavailable")

    monkeypatch.setenv("FACTORY_PROVIDER_KEY", API_KEY)
    with pytest.raises(SandboxError, match="Docker runtime is unavailable"):
        DockerSandboxAdapter(
            "factory-worker:test",
            command_runner=unavailable_docker,
        ).run(worktree, make_profile(), ("true",))

    assert len(calls) == 1
    assert calls[0][1] == "info"


def test_non_git_worktree_is_rejected_before_docker_is_called(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    worktree = tmp_path / "not-a-worktree"
    worktree.mkdir()
    fake_docker = FakeDocker()
    monkeypatch.setenv("FACTORY_PROVIDER_KEY", API_KEY)

    with pytest.raises(SandboxError, match="Git worktree root"):
        DockerSandboxAdapter(
            "factory-worker:test",
            command_runner=fake_docker,
        ).run(worktree, make_profile(), ("true",))

    assert fake_docker.calls == []
