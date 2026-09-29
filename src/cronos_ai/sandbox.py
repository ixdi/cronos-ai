"""Fail-closed Docker sandbox with a host-allowlisted egress gateway."""

from __future__ import annotations

import hashlib
import io
import json
import os
import select
import shutil
import subprocess
import tarfile
import tempfile
import threading
import time
from collections.abc import Callable, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import IO, Any
from uuid import uuid4

from cronos_ai.egress_gateway import canonicalize_host
from cronos_ai.models import SpecialistProfile


class SandboxError(RuntimeError):
    """Raised when required sandbox or egress controls cannot be established."""

    def __init__(self, message: str, *, transient: bool = False) -> None:
        self.transient = transient
        super().__init__(message)


@dataclass(frozen=True)
class SandboxResult:
    """A worker container's settled process result."""

    returncode: int
    stdout: str
    stderr: str


@dataclass(frozen=True)
class _SandboxRuntime:
    worktree: Path
    api_key: str
    worker_uid: int
    worker_gid: int
    proxy_uid: int
    gateway_name: str
    gateway_id: str
    worker_name: str


CommandRunner = Callable[..., subprocess.CompletedProcess[str]]
StreamProcessFactory = Callable[..., subprocess.Popen[bytes]]

_GATEWAY_DOCKERFILE = (
    "FROM python@sha256:"
    "a190708a2dec1bd18b1decb539f8e8f5407abaa9bf39cacda583f7f8c11db322\n"
    "RUN apk add --no-cache iptables=1.8.11-r1 su-exec=0.2-r3\n"
    "COPY egress_gateway.py /opt/egress_gateway.py\n"
    "COPY gateway-entrypoint.sh /usr/local/sbin/factory-egress-entrypoint\n"
    "RUN chmod 0555 /usr/local/sbin/factory-egress-entrypoint "
    "/opt/egress_gateway.py\n"
    'ENTRYPOINT ["/usr/local/sbin/factory-egress-entrypoint"]\n'
)

_GATEWAY_ENTRYPOINT = """#!/bin/sh
set -eu
proxy_uid="${FACTORY_PROXY_UID:?missing proxy uid}"
case "$proxy_uid" in
  ''|*[!0-9]*) exit 70 ;;
esac
iptables -w -F OUTPUT
iptables -w -P OUTPUT DROP
iptables -w -A OUTPUT -o lo -j ACCEPT
dns_count=0
while read -r directive nameserver rest; do
  [ "$directive" = "nameserver" ] || continue
  case "$nameserver" in
    *:*)
      ip6tables -w -A OUTPUT -m owner --uid-owner "$proxy_uid" \\
        -d "$nameserver" -p udp --dport 53 -j ACCEPT
      ip6tables -w -A OUTPUT -m owner --uid-owner "$proxy_uid" \\
        -d "$nameserver" -p tcp --dport 53 -j ACCEPT
      ;;
    *)
      iptables -w -A OUTPUT -m owner --uid-owner "$proxy_uid" \\
        -d "$nameserver" -p udp --dport 53 -j ACCEPT
      iptables -w -A OUTPUT -m owner --uid-owner "$proxy_uid" \\
        -d "$nameserver" -p tcp --dport 53 -j ACCEPT
      ;;
  esac
  dns_count=$((dns_count + 1))
done < /etc/resolv.conf
[ "$dns_count" -gt 0 ] || exit 70
iptables -w -A OUTPUT \\
  -m owner --uid-owner "$proxy_uid" -p tcp --dport 443 -j ACCEPT
ip6tables -w -F OUTPUT
ip6tables -w -P OUTPUT DROP
ip6tables -w -A OUTPUT -o lo -j ACCEPT
exec su-exec "$proxy_uid:$proxy_uid" python3 /opt/egress_gateway.py
"""

_WORKER_BOOTSTRAP = """set -eu
resource_root="$1"
shift
mkdir -p "$resource_root"
printf '__FACTORY_SANDBOX_READY__\\n'
IFS= read -r startup_token
[ "$startup_token" = "__FACTORY_SANDBOX_GO__" ] || exit 71
exec "$@"
"""


class SandboxProcess:
    """Popen-compatible handle that also owns the egress gateway lifecycle."""

    def __init__(
        self,
        process: subprocess.Popen[bytes],
        adapter: DockerSandboxAdapter,
        *,
        gateway_name: str,
        worker_name: str,
    ) -> None:
        self._process = process
        self._adapter = adapter
        self.gateway_name = gateway_name
        self.worker_name = worker_name
        self._cleanup_lock = threading.Lock()
        self._cleaned = False

    @property
    def stdin(self) -> IO[bytes] | None:
        return self._process.stdin

    @property
    def stdout(self) -> IO[bytes] | None:
        return self._process.stdout

    @property
    def stderr(self) -> IO[bytes] | None:
        return self._process.stderr

    @property
    def returncode(self) -> int | None:
        return self._process.returncode

    def poll(self) -> int | None:
        status = self._process.poll()
        if status is not None:
            self._cleanup()
        return status

    def wait(self, timeout: float | None = None) -> int:
        status = self._process.wait(timeout=timeout)
        self._cleanup()
        return status

    def communicate(
        self,
        input: bytes | None = None,
        timeout: float | None = None,
    ) -> tuple[bytes | None, bytes | None]:
        try:
            return self._process.communicate(input=input, timeout=timeout)
        finally:
            if self._process.poll() is not None:
                self._cleanup()

    def terminate(self) -> None:
        self._adapter._call(["stop", "--time=1", self.worker_name])
        if self._process.poll() is None:
            self._process.terminate()
            try:
                self._process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                self._process.kill()
                self._process.wait()
        self._cleanup()

    def kill(self) -> None:
        self._adapter._call(["rm", "-f", self.worker_name])
        if self._process.poll() is None:
            self._process.kill()
            self._process.wait()
        self._cleanup()

    def close(self) -> None:
        if self._process.poll() is None:
            if self._process.stdin is not None and not self._process.stdin.closed:
                self._process.stdin.close()
            try:
                self._process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                self.terminate()
                return
        self._cleanup()

    def _cleanup(self) -> None:
        with self._cleanup_lock:
            if self._cleaned:
                return
            self._cleaned = True
        self._adapter._cleanup_gateway(self.gateway_name)


class DockerSandboxAdapter:
    """Launch least-privilege worker containers behind an egress gateway."""

    def __init__(
        self,
        worker_image: str,
        *,
        docker_executable: str = "docker",
        command_runner: CommandRunner | None = None,
        process_factory: StreamProcessFactory | None = None,
        startup_timeout: float = 15.0,
    ) -> None:
        if not worker_image.strip():
            raise ValueError("worker image must not be empty")
        if startup_timeout <= 0:
            raise ValueError("sandbox startup timeout must be positive")
        self.worker_image = worker_image
        self.docker_executable = docker_executable
        self._runner = command_runner or subprocess.run
        self._process_factory = process_factory or subprocess.Popen
        self.startup_timeout = startup_timeout

    def _call(
        self,
        args: Sequence[str],
        *,
        cwd: Path | None = None,
    ) -> subprocess.CompletedProcess[str]:
        command = [self.docker_executable, *args]
        try:
            return self._runner(
                command,
                cwd=cwd,
                capture_output=True,
                text=True,
                check=False,
            )
        except OSError as error:
            raise SandboxError("Docker CLI is unavailable") from error

    @staticmethod
    def _allowed_hosts(profile: SpecialistProfile) -> tuple[str, ...]:
        hosts: set[str] = set()
        for destination in profile.network_allowlist:
            if (
                destination.scheme != "https"
                or destination.port not in (None, 443)
                or destination.path not in ("", "/")
                or destination.query is not None
                or destination.fragment is not None
                or destination.username is not None
                or destination.password is not None
            ):
                raise SandboxError(
                    "egress destinations must be HTTPS origins without credentials"
                )
            if destination.host is None:
                raise SandboxError("egress destination must include a hostname")
            hosts.add(canonicalize_host(destination.host))
        return tuple(sorted(hosts))

    def _gateway_image(self) -> str:
        gateway_source = Path(__file__).with_name("egress_gateway.py").read_bytes()
        digest = hashlib.sha256(
            _GATEWAY_DOCKERFILE.encode()
            + _GATEWAY_ENTRYPOINT.encode()
            + gateway_source
        ).hexdigest()[:20]
        image = f"cronos-ai-egress:sha256-{digest}"
        inspect = self._call(
            ["image", "inspect", "--format={{.Id}}", image]
        )
        if inspect.returncode == 0 and inspect.stdout.strip():
            return inspect.stdout.strip()

        with tempfile.TemporaryDirectory(prefix="cronos-ai-egress-build-") as temp_dir:
            context = Path(temp_dir)
            shutil.copyfile(
                Path(__file__).with_name("egress_gateway.py"),
                context / "egress_gateway.py",
            )
            (context / "Dockerfile").write_text(
                _GATEWAY_DOCKERFILE,
                encoding="utf-8",
            )
            (context / "gateway-entrypoint.sh").write_text(
                _GATEWAY_ENTRYPOINT,
                encoding="utf-8",
            )
            build = self._call(
                ["build", "--pull", "--tag", image, "--file", "Dockerfile", "."],
                cwd=context,
            )
        if build.returncode != 0:
            raise SandboxError("could not build the required egress gateway image")
        inspect = self._call(
            ["image", "inspect", "--format={{.Id}}", image]
        )
        if inspect.returncode != 0 or not inspect.stdout.strip():
            raise SandboxError("built egress gateway image could not be inspected")
        return inspect.stdout.strip()

    @staticmethod
    def _validate_worktree(worktree_path: Path) -> Path:
        try:
            worktree = worktree_path.resolve(strict=True)
        except OSError as error:
            raise SandboxError("task worktree does not exist") from error
        if not worktree.is_dir():
            raise SandboxError("task worktree must be a directory")
        try:
            result = subprocess.run(
                ["git", "rev-parse", "--show-toplevel"],
                cwd=worktree,
                capture_output=True,
                text=True,
                check=False,
            )
        except OSError as error:
            raise SandboxError("Git is unavailable") from error
        if result.returncode != 0 or Path(result.stdout.strip()).resolve() != worktree:
            raise SandboxError("sandbox mount must be the task Git worktree root")
        return worktree

    def _prepare_runtime(
        self,
        worktree_path: Path,
        profile: SpecialistProfile,
    ) -> _SandboxRuntime:
        api_key = os.environ.get(profile.provider_api_key_env)
        if not api_key:
            raise SandboxError(
                "configured provider API key is missing: "
                f"{profile.provider_api_key_env}"
            )
        worktree = self._validate_worktree(worktree_path)
        file_owner = worktree.stat()
        worker_uid = file_owner.st_uid
        worker_gid = file_owner.st_gid
        if worker_uid == 0:
            raise SandboxError("task worktree must not be owned by the root user")
        proxy_uid = 10001
        while proxy_uid == worker_uid:
            proxy_uid += 1

        docker_info = self._call(["info", "--format", "{{.ServerVersion}}"])
        if docker_info.returncode != 0:
            raise SandboxError("Docker runtime is unavailable", transient=True)

        gateway_name = f"factory-egress-{os.getpid()}-{uuid4().hex[:12]}"
        worker_name = f"factory-worker-{os.getpid()}-{uuid4().hex[:12]}"
        gateway_started = False
        try:
            allowed_hosts = self._allowed_hosts(profile)
            gateway_image = self._gateway_image()
            gateway = self._call(
                [
                    "run",
                    "--detach",
                    "--rm",
                    f"--name={gateway_name}",
                    "--network=bridge",
                    "--cap-drop=ALL",
                    "--cap-add=NET_ADMIN",
                    "--cap-add=SETUID",
                    "--cap-add=SETGID",
                    "--read-only",
                    "--pids-limit=64",
                    "--memory=256m",
                    "--cpus=0.5",
                    "--security-opt=no-new-privileges:true",
                    "--tmpfs=/tmp:rw,noexec,nosuid,nodev,size=16m",
                    f"--env=FACTORY_PROXY_UID={proxy_uid}",
                    f"--env=FACTORY_EGRESS_ALLOWLIST={json.dumps(allowed_hosts)}",
                    gateway_image,
                ]
            )
            if gateway.returncode != 0:
                raise SandboxError("could not start the egress gateway")
            gateway_started = True
            gateway_id = gateway.stdout.strip()
            if not gateway_id:
                raise SandboxError("egress gateway returned no container identifier")
            self._wait_for_gateway(gateway_name)
        except BaseException:
            if gateway_started:
                self._cleanup_gateway(gateway_name)
            raise

        return _SandboxRuntime(
            worktree=worktree,
            api_key=api_key,
            worker_uid=worker_uid,
            worker_gid=worker_gid,
            proxy_uid=proxy_uid,
            gateway_name=gateway_name,
            gateway_id=gateway_id,
            worker_name=worker_name,
        )

    def _worker_args(
        self,
        runtime: _SandboxRuntime,
        profile: SpecialistProfile,
        command: Sequence[str],
        *,
        interactive: bool,
        bootstrap: bool = False,
        container_resource_root: str | None = None,
        extra_environment: dict[str, str] | None = None,
    ) -> list[str]:
        args = ["run", "--rm"]
        if interactive:
            args.append("--interactive")
        args.extend(
            [
                f"--name={runtime.worker_name}",
                f"--network=container:{runtime.gateway_id}",
                f"--mount=type=bind,source={runtime.worktree},target=/workspace",
                "--read-only",
                "--tmpfs=/tmp:rw,noexec,nosuid,nodev,size=512m",
                f"--user={runtime.worker_uid}:{runtime.worker_gid}",
                "--cap-drop=ALL",
                "--security-opt=no-new-privileges:true",
                "--pids-limit=256",
                "--memory=2g",
                "--cpus=2",
                "--workdir=/workspace",
                "--env=HOME=/tmp/pi-home",
                "--env=TMPDIR=/tmp",
                "--env=XDG_CONFIG_HOME=/tmp/pi-config",
                "--env=XDG_CACHE_HOME=/tmp/pi-cache",
                "--env=PI_CODING_AGENT_DIR=/tmp/pi-agent",
                "--env=PI_CODING_AGENT_SESSION_DIR=/tmp/pi-sessions",
                "--env=PI_OFFLINE=1",
                "--env=NODE_USE_ENV_PROXY=1",
                "--env=HTTPS_PROXY=http://127.0.0.1:3128",
                "--env=HTTP_PROXY=http://127.0.0.1:3128",
                "--env=ALL_PROXY=http://127.0.0.1:3128",
                "--env=NO_PROXY=localhost,127.0.0.1",
                f"--env={profile.provider_api_key_env}",
            ]
        )
        if extra_environment:
            for name, value in sorted(extra_environment.items()):
                if not name or "=" in name or "\0" in name or "\0" in value:
                    raise SandboxError("sandbox environment contains an invalid entry")
                args.append(f"--env={name}={value}")
        if container_resource_root is not None:
            args.append("--entrypoint=/bin/sh")
        args.append(self.worker_image)
        if bootstrap:
            if container_resource_root is None:
                raise SandboxError("sandbox bootstrap requires a resource directory")
            args.extend(
                [
                    "-c",
                    _WORKER_BOOTSTRAP,
                    "factory-sandbox",
                    container_resource_root,
                ]
            )
        args.extend(command)
        return args

    def _wait_for_gateway(self, name: str) -> None:
        probe = [
            "exec",
            name,
            "python3",
            "-c",
            "import socket; "
            "socket.create_connection(('127.0.0.1', 3128), timeout=1).close()",
        ]
        for _ in range(30):
            result = self._call(probe)
            if result.returncode == 0:
                return
            time.sleep(0.1)
        raise SandboxError("egress gateway failed to establish its enforced policy")

    @staticmethod
    def _resource_arguments(
        command: Sequence[str],
        environment: dict[str, str],
        resource_root: Path,
    ) -> tuple[list[str], list[tuple[Path, str]], dict[str, str]]:
        rewritten = list(command)
        copies: list[tuple[Path, str]] = []
        runtime_root = resource_root
        profile_root_value = environment.get("FACTORY_PROFILE_ROOT")
        skill_root = (
            Path(profile_root_value).resolve(strict=True)
            if profile_root_value is not None
            else None
        )

        index = 0
        while index < len(rewritten):
            option = rewritten[index]
            if option not in ("--skill", "--extension"):
                index += 1
                continue
            if index + 1 >= len(rewritten):
                raise SandboxError(f"Pi resource option has no path: {option}")
            source_argument = Path(rewritten[index + 1])
            if source_argument.is_symlink():
                raise SandboxError("Pi resources must not be symbolic links")
            try:
                source = source_argument.resolve(strict=True)
            except OSError as error:
                raise SandboxError("selected Pi resource does not exist") from error

            if option == "--skill":
                if skill_root is None:
                    raise SandboxError(
                        "Pi skill is not from a resolved factory profile"
                    )
                allowed_root = (skill_root / "skills").resolve(strict=True)
                if not source.is_relative_to(allowed_root) or not source.is_dir():
                    raise SandboxError("Pi skill is outside the factory skill root")
                for child in source.rglob("*"):
                    if child.is_symlink() or not child.resolve().is_relative_to(source):
                        raise SandboxError("Pi skill contains an unsafe path")
                destination = f"{runtime_root}/skill-{len(copies)}"
                copies.append((source, destination))
            else:
                mcp_config = environment.get("FACTORY_MCP_CONFIG_PATH")
                if (
                    mcp_config is None
                    or source.name != "factory-mcp-bridge.ts"
                    or source.parent != Path(mcp_config).resolve(strict=True).parent
                ):
                    raise SandboxError(
                        "only the factory MCP bridge extension is permitted"
                    )
                destination = f"{runtime_root}/factory-mcp-bridge.ts"
                copies.append((source, destination))
            rewritten[index + 1] = destination
            index += 2

        mcp_config = environment.get("FACTORY_MCP_CONFIG_PATH")
        child_environment: dict[str, str] = {}
        if mcp_config is not None:
            config_source = Path(mcp_config).resolve(strict=True)
            if config_source.name != "factory-mcp-config.json":
                raise SandboxError("MCP configuration file is not factory managed")
            config_destination = f"{runtime_root}/factory-mcp-config.json"
            copies.append((config_source, config_destination))
            child_environment["FACTORY_MCP_CONFIG_PATH"] = config_destination
        return rewritten, copies, child_environment

    def open_process(
        self,
        worktree_path: Path,
        profile: SpecialistProfile,
        command: Sequence[str],
        *,
        environment: dict[str, str] | None = None,
    ) -> SandboxProcess:
        """Start an interactive worker process with isolated stdin/stdout pipes."""
        if not command or any(not part or "\0" in part for part in command):
            raise SandboxError("worker command must contain non-empty arguments")
        process_environment = dict(environment or {})
        runtime = self._prepare_runtime(worktree_path, profile)
        resource_path = f"/tmp/factory-resources-{uuid4().hex[:12]}"
        container_name: str | None = None
        process: subprocess.Popen[bytes] | None = None
        try:
            worker_command, resource_copies, worker_environment = (
                self._resource_arguments(
                command,
                process_environment,
                    Path(resource_path),
                )
            )
            args = self._worker_args(
                runtime,
                profile,
                worker_command,
                interactive=True,
                bootstrap=True,
                container_resource_root=resource_path,
                extra_environment=worker_environment,
            )
            try:
                process = self._process_factory(
                    [self.docker_executable, *args],
                    cwd=runtime.worktree,
                    env=os.environ.copy(),
                    stdin=subprocess.PIPE,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    bufsize=0,
                    start_new_session=True,
                )
            except OSError as error:
                raise SandboxError("could not start Docker worker process") from error
            container_name = self._wait_for_worker_bootstrap(process)
            self._copy_profile_resources(
                container_name,
                resource_path,
                resource_copies,
            )
            if process.stdin is None:
                raise SandboxError("Docker worker has no stdin pipe")
            process.stdin.write(b"__FACTORY_SANDBOX_GO__\n")
            process.stdin.flush()
            return SandboxProcess(
                process,
                self,
                gateway_name=runtime.gateway_name,
                worker_name=runtime.worker_name,
            )
        except BaseException:
            self._call(["rm", "-f", runtime.worker_name])
            if process is not None and process.poll() is None:
                process.terminate()
                process.wait()
            self._cleanup_gateway(runtime.gateway_name)
            raise

    def process_factory(
        self,
        worktree_path: Path,
        profile: SpecialistProfile,
    ) -> Callable[..., SandboxProcess]:
        """Create a Pi process factory that runs its RPC child inside Docker."""
        def start_worker(command: Sequence[str], **kwargs: Any) -> SandboxProcess:
            environment = kwargs.get("env")
            if not isinstance(environment, dict):
                raise SandboxError("Pi process factory did not provide an environment")
            return self.open_process(
                worktree_path,
                profile,
                command,
                environment=environment,
            )

        return start_worker

    def _wait_for_worker_bootstrap(
        self,
        process: subprocess.Popen[bytes],
    ) -> str:
        if process.stdout is None:
            raise SandboxError("Docker worker has no stdout pipe")
        readable, _, _ = select.select([process.stdout], [], [], self.startup_timeout)
        if not readable:
            raise SandboxError("Docker worker bootstrap timed out")
        ready_line = process.stdout.readline()
        if ready_line != b"__FACTORY_SANDBOX_READY__\n":
            raise SandboxError("Docker worker failed during sandbox bootstrap")
        # Docker CLI does not expose the container id through --interactive.
        # The unique name is the stable cleanup and copy target.
        return self._container_name_from_process(process)

    @staticmethod
    def _container_name_from_process(process: subprocess.Popen[bytes]) -> str:
        args = getattr(process, "args", None)
        if not isinstance(args, (list, tuple)):
            raise SandboxError("Docker process arguments are unavailable")
        name: str | None = None
        for argument in args:
            if isinstance(argument, str) and argument.startswith("--name="):
                name = argument.split("=", 1)[1]
                break
        if name is None:
            raise SandboxError("Docker worker container name is unavailable")
        return name

    def _copy_profile_resources(
        self,
        container: str,
        resource_root: str,
        copies: Sequence[tuple[Path, str]],
    ) -> None:
        if not copies:
            return
        archive_buffer = io.BytesIO()
        try:
            with tarfile.open(fileobj=archive_buffer, mode="w") as archive:
                for source, destination in copies:
                    relative_destination = Path(destination).relative_to("/")
                    archive.add(
                        source,
                        arcname=relative_destination.as_posix(),
                        recursive=source.is_dir(),
                    )
        except (OSError, tarfile.TarError, ValueError) as error:
            raise SandboxError(
                "could not archive approved profile resources"
            ) from error

        if len(archive_buffer.getbuffer()) > 64 * 1024 * 1024:
            raise SandboxError("approved profile resources exceed size limit")
        command = [
            self.docker_executable,
            "exec",
            "-i",
            container,
            "tar",
            "-xf",
            "-",
            "-C",
            "/",
        ]
        try:
            copied = subprocess.run(
                command,
                input=archive_buffer.getvalue(),
                capture_output=True,
                check=False,
            )
        except OSError as error:
            raise SandboxError("Docker could not copy profile resources") from error
        if copied.returncode != 0:
            details = copied.stderr.decode("utf-8", errors="replace").strip()
            message = "could not copy approved resources into worker sandbox"
            if details:
                message = f"{message}: {details}"
            raise SandboxError(message)

    def _cleanup_gateway(self, gateway_name: str) -> None:
        cleanup = self._call(["rm", "-f", gateway_name])
        if cleanup.returncode != 0:
            raise SandboxError("could not remove the egress gateway container")

    def run(
        self,
        worktree_path: Path,
        profile: SpecialistProfile,
        command: Sequence[str],
    ) -> SandboxResult:
        """Run a command with only the task worktree and configured key exposed."""
        runtime = self._prepare_runtime(worktree_path, profile)
        try:
            worker_args = self._worker_args(
                runtime,
                profile,
                command,
                interactive=False,
            )
            worker_command = self._call(worker_args, cwd=runtime.worktree)
            return SandboxResult(
                returncode=worker_command.returncode,
                stdout=worker_command.stdout.replace(runtime.api_key, "[REDACTED]"),
                stderr=worker_command.stderr.replace(runtime.api_key, "[REDACTED]"),
            )
        finally:
            self._cleanup_gateway(runtime.gateway_name)
