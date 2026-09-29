"""Long-running local controller and state-directory configuration."""

from __future__ import annotations

import ctypes
import errno
import fcntl
import os
import stat
import sys
from datetime import UTC, datetime
from pathlib import Path
from threading import Event

from cronos_ai.attention import HumanActionProcessor
from cronos_ai.models import ControllerLifecycle, ControllerStatus
from cronos_ai.storage import FactoryStore


class StateMigrationError(RuntimeError):
    """Raised when default-state migration cannot proceed safely."""


def _validate_state_root(root: Path) -> None:
    info = root.stat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid():
        raise StateMigrationError(
            f"state root {root} must be a directory owned by the current user"
        )
    if info.st_mode & (stat.S_IWGRP | stat.S_IWOTH):
        raise StateMigrationError(
            f"state root {root} is writable by other users; refusing migration"
        )


def _migration_lock(root: Path) -> int:
    lock_path = root / ".cronos-ai-state-migration.lock"
    flags = os.O_CREAT | os.O_RDWR
    flags |= getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(lock_path, flags, 0o600)
    try:
        info = os.fstat(descriptor)
        if (
            not stat.S_ISREG(info.st_mode)
            or info.st_uid != os.getuid()
            or stat.S_IMODE(info.st_mode) & 0o077
        ):
            raise StateMigrationError(
                f"migration lock {lock_path} is not a private file "
                "owned by the current user"
            )
        fcntl.flock(descriptor, fcntl.LOCK_EX)
    except BaseException:
        os.close(descriptor)
        raise
    return descriptor


def _existing_entry(path: Path) -> os.stat_result | None:
    try:
        return path.lstat()
    except FileNotFoundError:
        return None


def _validate_state_directory(path: Path, info: os.stat_result) -> None:
    if stat.S_ISLNK(info.st_mode):
        raise StateMigrationError(
            f"state directory {path} is a symbolic link; refusing automatic migration"
        )
    if not stat.S_ISDIR(info.st_mode):
        raise StateMigrationError(
            f"state path {path} is not a directory; refusing automatic migration"
        )
    if info.st_uid != os.getuid():
        raise StateMigrationError(
            f"state directory {path} is not owned by the current user; "
            "refusing automatic migration"
        )


def _rename_directory_no_replace(source: Path, destination: Path) -> None:
    library = ctypes.CDLL(None, use_errno=True)
    if sys.platform == "darwin":
        rename = getattr(library, "renamex_np", None)
        if rename is None:
            raise StateMigrationError(
                "this system has no safe no-clobber directory rename; "
                f"move {source} to {destination} manually"
            )
        rename.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_uint]
        rename.restype = ctypes.c_int
        result = rename(os.fsencode(source), os.fsencode(destination), 0x00000004)
    elif sys.platform.startswith("linux"):
        rename = getattr(library, "renameat2", None)
        if rename is None:
            raise StateMigrationError(
                "this system has no safe no-clobber directory rename; "
                f"move {source} to {destination} manually"
            )
        rename.argtypes = [
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_uint,
        ]
        rename.restype = ctypes.c_int
        result = rename(
            -100,
            os.fsencode(source),
            -100,
            os.fsencode(destination),
            1,
        )
    else:
        raise StateMigrationError(
            "this system has no supported safe no-clobber directory rename; "
            f"move {source} to {destination} manually"
        )

    if result == 0:
        return
    error_number = ctypes.get_errno()
    if error_number in {errno.EEXIST, errno.ENOTEMPTY}:
        raise StateMigrationError(
            f"destination {destination} appeared during migration; "
            f"the existing state at {source} was left unchanged"
        )
    raise StateMigrationError(
        f"could not move state directory {source} to {destination}: "
        f"{os.strerror(error_number)}"
    )


def _resolve_default_state_dir(root: Path) -> Path:
    try:
        root.mkdir(parents=True, exist_ok=True, mode=0o700)
        root = root.resolve(strict=True)
        _validate_state_root(root)
        lock_descriptor = _migration_lock(root)
    except StateMigrationError:
        raise
    except OSError as error:
        raise StateMigrationError(
            f"could not safely access state root {root}: {error}"
        ) from error

    try:
        legacy = root / "ai-software-factory"
        current = root / "cronos-ai"
        legacy_info = _existing_entry(legacy)
        current_info = _existing_entry(current)
        if legacy_info is not None and current_info is not None:
            raise StateMigrationError(
                f"both default state directories exist ({legacy} and {current}); "
                "refusing to merge or overwrite either directory. Back up both "
                "directories, choose the authoritative state, then move one "
                "directory out of this state root and retry."
            )
        if current_info is not None:
            _validate_state_directory(current, current_info)
            return current
        if legacy_info is None:
            return current

        _validate_state_directory(legacy, legacy_info)
        _rename_directory_no_replace(legacy, current)
        return current
    except StateMigrationError:
        raise
    except OSError as error:
        raise StateMigrationError(
            f"could not safely inspect or migrate state under {root}: {error}"
        ) from error
    finally:
        fcntl.flock(lock_descriptor, fcntl.LOCK_UN)
        os.close(lock_descriptor)


def default_state_dir() -> Path:
    """Resolve the configured state path or safely migrate the default path."""
    configured = os.environ.get("CRONOS_AI_STATE_DIR")
    if configured:
        return Path(configured).expanduser()
    xdg_state_home = os.environ.get("XDG_STATE_HOME")
    root = (
        Path(xdg_state_home).expanduser()
        if xdg_state_home
        else Path.home() / ".local" / "state"
    )
    return _resolve_default_state_dir(root)


class LocalController:
    """Own the factory's controller lock and durable lifecycle status."""

    def __init__(
        self,
        state_dir: Path,
        *,
        heartbeat_interval: float = 2.0,
        max_attempts: int = 3,
    ) -> None:
        if heartbeat_interval <= 0:
            raise ValueError("heartbeat interval must be positive")
        if max_attempts < 1:
            raise ValueError("max_attempts must be positive")
        self.state_dir = state_dir.expanduser()
        self.heartbeat_interval = heartbeat_interval
        self.max_attempts = max_attempts
        self._store: FactoryStore | None = None

    @property
    def store(self) -> FactoryStore:
        """Return the open store while this controller owns its lock."""
        if self._store is None:
            raise RuntimeError("controller is not running")
        return self._store

    def __enter__(self) -> LocalController:
        self.state_dir.mkdir(parents=True, exist_ok=True, mode=0o700)
        store = FactoryStore(self.state_dir / "factory.sqlite3")
        try:
            store.acquire_controller_lock()
            now = datetime.now(UTC)
            store.set_controller_status(
                ControllerStatus(
                    state=ControllerLifecycle.RUNNING,
                    pid=os.getpid(),
                    started_at=now,
                    heartbeat_at=now,
                )
            )
            HumanActionProcessor(
                store, max_attempts=self.max_attempts
            ).recover_interrupted()
        except BaseException:
            store.close()
            raise
        self._store = store
        return self

    def __exit__(self, *_: object) -> None:
        store = self._store
        if store is None:
            return
        try:
            current = store.get_controller_status()
            if current is not None:
                now = datetime.now(UTC)
                store.set_controller_status(
                    ControllerStatus(
                        state=ControllerLifecycle.STOPPED,
                        pid=current.pid,
                        started_at=current.started_at,
                        heartbeat_at=now,
                        stopped_at=now,
                    )
                )
        finally:
            store.close()
            self._store = None

    def heartbeat(self) -> None:
        """Persist a controller liveness timestamp."""
        current = self.store.get_controller_status()
        if current is None:
            raise RuntimeError("controller status has not been initialized")
        now = datetime.now(UTC)
        self.store.set_controller_status(
            ControllerStatus(
                state=ControllerLifecycle.RUNNING,
                pid=current.pid,
                started_at=current.started_at,
                heartbeat_at=now,
            )
        )

    def run_forever(self, stop_event: Event | None = None) -> None:
        """Hold the controller lock and update its heartbeat until stopped."""
        event = stop_event or Event()
        with self:
            processor = HumanActionProcessor(self.store, max_attempts=self.max_attempts)
            print(
                f"Factory controller running with PID {os.getpid()}. "
                "Press Ctrl+C to stop."
            )
            while not event.is_set():
                processor.process_pending()
                self.heartbeat()
                event.wait(self.heartbeat_interval)
