"""Supervise Pi RPC subprocesses and require settled completion events."""

from __future__ import annotations

import json
import os
import queue
import subprocess
import tempfile
import threading
import time
from collections.abc import Callable, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import IO, Any, Protocol
from uuid import uuid4

from cronos_ai.mcp_bridge import (
    MCPBridgeError,
    MCPBridgeFiles,
    prepare_mcp_bridge,
)
from cronos_ai.models import SpecialistProfile
from cronos_ai.profiles import (
    ResolvedSpecialistProfile,
    pi_mcp_tool_name,
)


class PiRpcError(RuntimeError):
    """Base error for a failed Pi RPC worker interaction."""


class PiProtocolError(PiRpcError):
    """Raised for malformed or unexpected records on the Pi RPC stream."""


class PiProcessExited(PiRpcError):
    """Raised when Pi exits before settling the active run."""


class PiCommandRejected(PiRpcError):
    """Raised when Pi rejects a prompt before accepting it."""


class PiRunTimeout(PiRpcError):
    """Raised when a Pi run does not report agent_settled by its deadline."""


class PiWorkerBlocked(PiRpcError):
    """Raised when Pi requests unsupported interactive extension UI input."""


@dataclass(frozen=True)
class PiRunResult:
    """A Pi run that received both prompt acceptance and agent_settled."""

    request_id: str
    events: tuple[dict[str, Any], ...]
    settled: bool


class RpcProcess(Protocol):
    @property
    def stdin(self) -> IO[bytes] | None: ...

    @property
    def stdout(self) -> IO[bytes] | None: ...

    @property
    def stderr(self) -> IO[bytes] | None: ...

    @property
    def returncode(self) -> int | None: ...

    def poll(self) -> int | None: ...

    def wait(self, timeout: float | None = None) -> int: ...

    def terminate(self) -> None: ...

    def kill(self) -> None: ...


ProcessFactory = Callable[..., RpcProcess]
EventHandler = Callable[[dict[str, Any]], None]
_EOF = object()
_MAX_RECORD_BYTES = 4 * 1024 * 1024


class PiRpcSupervisor:
    """Run one Pi RPC process and serialize prompts through settled completion."""

    def __init__(
        self,
        profile: SpecialistProfile,
        cwd: Path,
        *,
        resources: ResolvedSpecialistProfile | None = None,
        executable: str = "pi",
        command: Sequence[str] | None = None,
        process_factory: ProcessFactory | None = None,
        event_handler: EventHandler | None = None,
        max_record_bytes: int = _MAX_RECORD_BYTES,
        mcp_startup_timeout: float = 30.0,
    ) -> None:
        if max_record_bytes < 1024:
            raise ValueError("maximum Pi record size must be at least 1024 bytes")
        if resources is not None:
            if resources.profile != profile:
                raise ValueError(
                    "resolved resources do not match the specialist profile"
                )
            resource_root = resources.resource_root.resolve(strict=True)
            working_root = cwd.resolve()
            if (
                resource_root.is_relative_to(working_root)
                or working_root.is_relative_to(resource_root)
            ):
                raise ValueError(
                    "factory profile resources must be separate from the task worktree"
                )
            if resources.skill_paths:
                skills_root = (resource_root / "skills").resolve(strict=True)
                if any(
                    not skill_path.resolve(strict=True).is_relative_to(skills_root)
                    for skill_path in resources.skill_paths
                ):
                    raise ValueError(
                        "resolved skill path escapes factory resource root"
                    )
            resolved_tool_refs = {
                f"{server.definition.server_id}/{tool_name}"
                for server in resources.mcp_servers
                for tool_name in server.tools
            }
            if resolved_tool_refs != set(profile.mcp_tools):
                raise ValueError(
                    "resolved MCP tools do not match the specialist profile"
                )
        if resources is None and (profile.skills or profile.mcp_tools):
            raise ValueError(
                "profiles with skills or MCP tools must be resolved "
                "by the factory registry"
            )
        if mcp_startup_timeout <= 0:
            raise ValueError("MCP startup timeout must be positive")
        self.profile = profile
        self.resources = resources
        self.mcp_startup_timeout = mcp_startup_timeout
        self._mcp_tempdir: tempfile.TemporaryDirectory[str] | None = None
        self._mcp_bridge_files: MCPBridgeFiles | None = None
        self._active_mcp_tools: tuple[str, ...] = ()
        self.cwd = cwd.resolve()
        self.executable = executable
        self._command_override = tuple(command) if command is not None else None
        self._process_factory = process_factory or subprocess.Popen
        self._event_handler = event_handler
        self.max_record_bytes = max_record_bytes
        self._process: RpcProcess | None = None
        self._records: queue.Queue[object] | None = None
        self._reader_threads: list[threading.Thread] = []
        self._run_lock = threading.Lock()

    @property
    def process(self) -> RpcProcess | None:
        """Return the current child process, if one is running."""
        return self._process

    @property
    def active_mcp_tools(self) -> tuple[str, ...]:
        """Return the profile-approved MCP tools registered by Pi."""
        return self._active_mcp_tools

    @property
    def command(self) -> list[str]:
        """Return the argument vector used to start Pi without secret values."""
        if self._command_override is not None:
            return list(self._command_override)
        tool_names = ["read", "bash", "edit", "write"]
        skill_paths: tuple[Path, ...] = ()
        if self.resources is not None:
            skill_paths = self.resources.skill_paths
            for server in self.resources.mcp_servers:
                tool_names.extend(
                    pi_mcp_tool_name(server.definition.server_id, tool_name)
                    for tool_name in server.tools
                )
        command = [
            self.executable,
            "--mode",
            "rpc",
            "--no-session",
            "--provider",
            self.profile.provider,
            "--model",
            self.profile.model,
            "--tools",
            ",".join(tool_names),
            "--no-extensions",
            "--no-skills",
            "--no-prompt-templates",
            "--no-themes",
            "--no-context-files",
            "--no-approve",
        ]
        for skill_path in skill_paths:
            command.extend(("--skill", str(skill_path)))
        if self.resources is not None and self.resources.mcp_servers:
            bridge_files = self._prepare_mcp_bridge()
            command.extend(("--extension", str(bridge_files.extension_path)))
        return command

    def _redact(self, value: Any) -> Any:
        secret = os.environ.get(self.profile.provider_api_key_env)
        if not secret:
            return value
        if isinstance(value, str):
            return value.replace(secret, "[REDACTED]")
        if isinstance(value, list):
            return [self._redact(item) for item in value]
        if isinstance(value, tuple):
            return tuple(self._redact(item) for item in value)
        if isinstance(value, dict):
            return {
                self._redact(key): self._redact(item)
                for key, item in value.items()
            }
        return value

    def child_environment(self) -> dict[str, str]:
        """Build a minimal child environment with only the configured API key."""
        api_key = os.environ.get(self.profile.provider_api_key_env)
        if not api_key:
            raise PiRpcError(
                f"configured provider API key is missing: "
                f"{self.profile.provider_api_key_env}"
            )

        environment: dict[str, str] = {
            "PATH": os.environ.get("PATH", "/usr/local/bin:/usr/bin:/bin"),
            "HOME": "/tmp/pi-home",
            "TMPDIR": "/tmp",
            "XDG_CONFIG_HOME": "/tmp/pi-config",
            "XDG_CACHE_HOME": "/tmp/pi-cache",
            "PI_CODING_AGENT_DIR": "/tmp/pi-agent",
            "PI_CODING_AGENT_SESSION_DIR": "/tmp/pi-sessions",
            "NODE_USE_ENV_PROXY": "1",
            "PI_OFFLINE": "1",
            self.profile.provider_api_key_env: api_key,
        }
        if self.resources is not None:
            environment["FACTORY_PROFILE_ROOT"] = str(self.resources.resource_root)
        if self._mcp_bridge_files is not None:
            environment["FACTORY_MCP_CONFIG_PATH"] = str(
                self._mcp_bridge_files.config_path
            )
        for name in (
            "LANG",
            "LC_ALL",
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "NO_PROXY",
        ):
            value = os.environ.get(name)
            if value:
                environment[name] = value
        return environment

    @staticmethod
    def _read_stdout(
        stream: Any,
        records: queue.Queue[object],
        max_record_bytes: int,
    ) -> None:
        buffer = bytearray()
        try:
            while True:
                chunk = os.read(stream.fileno(), 65536)
                if not chunk:
                    if buffer:
                        records.put(
                            PiProtocolError("unterminated JSONL record from Pi")
                        )
                    else:
                        records.put(_EOF)
                    return
                buffer.extend(chunk)
                if len(buffer) > max_record_bytes and b"\n" not in buffer:
                    records.put(PiProtocolError("Pi JSONL record exceeded size limit"))
                    return
                while True:
                    newline = buffer.find(b"\n")
                    if newline < 0:
                        break
                    line = bytes(buffer[:newline])
                    del buffer[: newline + 1]
                    if line.endswith(b"\r"):
                        line = line[:-1]
                    if len(line) > max_record_bytes:
                        records.put(
                            PiProtocolError("Pi JSONL record exceeded size limit")
                        )
                        return
                    try:
                        record = json.loads(line)
                    except (UnicodeDecodeError, json.JSONDecodeError) as error:
                        raise PiProtocolError("Pi emitted invalid JSONL") from error
                    if not isinstance(record, dict):
                        raise PiProtocolError("Pi JSONL record must be an object")
                    records.put(record)
        except BaseException as error:
            records.put(error)

    @staticmethod
    def _drain_stderr(stream: Any) -> None:
        try:
            while os.read(stream.fileno(), 65536):
                pass
        except OSError:
            return

    def _prepare_mcp_bridge(self) -> MCPBridgeFiles:
        if self.resources is None:
            raise PiRpcError("MCP resources were not resolved by the factory")
        if self._mcp_bridge_files is not None:
            return self._mcp_bridge_files
        self._mcp_tempdir = tempfile.TemporaryDirectory(prefix="pi-mcp-bridge-")
        try:
            self._mcp_bridge_files = prepare_mcp_bridge(
                self.resources,
                Path(self._mcp_tempdir.name),
            )
        except MCPBridgeError as error:
            self._mcp_tempdir.cleanup()
            self._mcp_tempdir = None
            raise PiRpcError("could not prepare the curated MCP bridge") from error
        return self._mcp_bridge_files

    def _wait_for_mcp_bridge(
        self,
        process: RpcProcess,
        records: queue.Queue[object],
    ) -> None:
        deadline = time.monotonic() + self.mcp_startup_timeout
        while time.monotonic() < deadline:
            try:
                record = records.get(timeout=min(deadline - time.monotonic(), 0.1))
            except queue.Empty:
                if process.poll() is not None:
                    raise PiProcessExited(
                        "Pi exited before MCP tools were initialized"
                    ) from None
                continue
            if record is _EOF:
                raise PiProcessExited("Pi exited before MCP tools were initialized")
            if isinstance(record, BaseException):
                raise record
            if not isinstance(record, dict):
                raise PiProtocolError("Pi stream yielded an invalid startup record")
            if record.get("type") == "extension_error":
                raise PiProtocolError("a Pi extension failed during MCP initialization")
            if record.get("type") == "extension_ui_request":
                if (
                    record.get("method") == "setStatus"
                    and record.get("statusKey") == "factory-mcp"
                ):
                    try:
                        status = json.loads(record.get("statusText", ""))
                    except json.JSONDecodeError as error:
                        raise PiProtocolError(
                            "MCP extension returned invalid readiness data"
                        ) from error
                    if status.get("state") == "ready":
                        tools = status.get("tools", [])
                        if not isinstance(tools, list) or not all(
                            isinstance(tool, str) for tool in tools
                        ):
                            raise PiProtocolError(
                                "MCP extension returned an invalid tool list"
                            )
                        self._active_mcp_tools = tuple(tools)
                        return
                    raise PiProtocolError("factory MCP bridge failed to initialize")
                self._handle_extension_ui_request(record)
        raise PiRunTimeout("factory MCP bridge did not become ready")

    @staticmethod
    def _handle_extension_ui_request(record: dict[str, Any]) -> None:
        if record.get("method") in {
            "notify",
            "setStatus",
            "setWidget",
            "setTitle",
            "set_editor_text",
        }:
            return
        raise PiWorkerBlocked("Pi extension requested unsupported user interaction")

    def _start(self) -> RpcProcess:
        if self._process is not None:
            status = self._process.poll()
            if status is None:
                return self._process
            self._process = None
            self._records = None
            self._reader_threads = []

        if not self.cwd.is_dir():
            raise PiRpcError("Pi working directory does not exist")
        try:
            command = self.command
            environment = self.child_environment()
            process = self._process_factory(
                command,
                cwd=self.cwd,
                env=environment,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                bufsize=0,
                start_new_session=True,
            )
        except OSError as error:
            self._cleanup_mcp_files()
            raise PiRpcError("could not start Pi RPC subprocess") from error
        except BaseException:
            self._cleanup_mcp_files()
            raise
        if process.stdin is None or process.stdout is None or process.stderr is None:
            process.kill()
            process.wait()
            self._cleanup_mcp_files()
            raise PiRpcError("Pi RPC subprocess did not expose required pipes")

        records: queue.Queue[object] = queue.Queue()
        stdout_thread = threading.Thread(
            target=self._read_stdout,
            args=(process.stdout, records, self.max_record_bytes),
            daemon=True,
            name="pi-rpc-stdout",
        )
        stderr_thread = threading.Thread(
            target=self._drain_stderr,
            args=(process.stderr,),
            daemon=True,
            name="pi-rpc-stderr",
        )
        stdout_thread.start()
        stderr_thread.start()
        self._process = process
        self._records = records
        self._reader_threads = [stdout_thread, stderr_thread]
        if self.resources is not None and self.resources.mcp_servers:
            try:
                self._wait_for_mcp_bridge(process, records)
            except BaseException:
                self._stop()
                raise
        return process

    def _stop(self, *, graceful_timeout: float = 1.0) -> None:
        process = self._process
        self._process = None
        self._records = None
        self._reader_threads = []
        if process is None:
            self._cleanup_mcp_files()
            return
        if process.stdin is not None and not process.stdin.closed:
            try:
                process.stdin.close()
            except OSError:
                pass
        try:
            process.wait(timeout=graceful_timeout)
        except subprocess.TimeoutExpired:
            process.terminate()
            try:
                process.wait(timeout=graceful_timeout)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
        finally:
            self._cleanup_mcp_files()

    def _cleanup_mcp_files(self) -> None:
        tempdir = self._mcp_tempdir
        self._mcp_tempdir = None
        self._mcp_bridge_files = None
        self._active_mcp_tools = ()
        if tempdir is not None:
            tempdir.cleanup()

    def start(self) -> RpcProcess:
        """Start Pi RPC and wait for configured MCP tools to register."""
        with self._run_lock:
            return self._start()

    def run_prompt(self, prompt: str, *, timeout: float = 1800.0) -> PiRunResult:
        """Send one prompt and return only after its agent_settled event."""
        if not prompt.strip():
            raise ValueError("Pi prompt must not be empty")
        if timeout <= 0:
            raise ValueError("Pi run timeout must be positive")

        with self._run_lock:
            process = self._start()
            assert process.stdin is not None
            records = self._records
            assert records is not None
            request_id = f"factory-prompt-{uuid4().hex}"
            command = {
                "id": request_id,
                "type": "prompt",
                "message": prompt,
            }
            try:
                process.stdin.write(
                    json.dumps(command, separators=(",", ":")).encode("utf-8")
                    + b"\n"
                )
                process.stdin.flush()
            except OSError as error:
                self._stop()
                raise PiProcessExited("could not submit prompt to Pi") from error

            accepted = False
            settled = False
            events: list[dict[str, Any]] = []
            deadline = time.monotonic() + timeout
            try:
                while not (accepted and settled):
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        raise PiRunTimeout(
                            "Pi did not report agent_settled before the deadline"
                        )
                    try:
                        record = records.get(timeout=min(remaining, 0.1))
                    except queue.Empty:
                        if process.poll() is not None:
                            raise PiProcessExited(
                                "Pi exited before agent_settled"
                            ) from None
                        continue

                    if record is _EOF:
                        raise PiProcessExited("Pi exited before agent_settled")
                    if isinstance(record, BaseException):
                        raise record
                    if not isinstance(record, dict):
                        raise PiProtocolError("Pi stream yielded an invalid record")

                    if record.get("type") == "response":
                        if record.get("id") != request_id:
                            raise PiProtocolError(
                                "Pi returned a response for an unknown command"
                            )
                        if record.get("command") != "prompt":
                            raise PiProtocolError(
                                "Pi returned a response for an unexpected command"
                            )
                        if record.get("success") is not True:
                            detail = self._redact(
                                record.get("error", "prompt rejected")
                            )
                            raise PiCommandRejected(str(detail))
                        accepted = True
                        continue

                    if record.get("type") == "extension_error":
                        raise PiProtocolError("a Pi extension reported an error")
                    if record.get("type") == "extension_ui_request":
                        self._handle_extension_ui_request(record)
                        continue
                    safe_record = self._redact(record)
                    events.append(safe_record)
                    if self._event_handler is not None:
                        self._event_handler(safe_record)
                    if record.get("type") == "agent_settled":
                        settled = True

                return PiRunResult(
                    request_id=request_id,
                    events=tuple(events),
                    settled=True,
                )
            except (PiRpcError, OSError, queue.Empty):
                self._stop()
                raise
            except BaseException:
                self._stop()
                raise

    def close(self) -> None:
        """Close stdin and request an orderly Pi process shutdown."""
        with self._run_lock:
            self._stop()

    def __enter__(self) -> PiRpcSupervisor:
        return self

    def __exit__(self, *_: object) -> None:
        self.close()
