import json
import sys
from pathlib import Path
from typing import Any

import pytest

from cronos_ai.models import SpecialistProfile
from cronos_ai.pi_rpc import (
    PiCommandRejected,
    PiProcessExited,
    PiProtocolError,
    PiRpcSupervisor,
    PiRunTimeout,
)

FAKE_PI = r'''
import json
import os
import sys
import time

scenario = sys.argv[1]
for line in sys.stdin:
    command = json.loads(line)
    if command.get("type") != "prompt":
        continue
    response = {
        "id": command["id"],
        "type": "response",
        "command": "prompt",
        "success": scenario != "rejected",
    }
    if scenario == "rejected":
        response["error"] = "prompt rejected"
    print(json.dumps(response), flush=True)
    if scenario == "malformed":
        print("not-json", flush=True)
        break
    if scenario == "exit":
        raise SystemExit(23)
    if scenario == "unsettled":
        print(
            json.dumps({"type": "agent_end", "messages": [], "willRetry": False}),
            flush=True,
        )
        time.sleep(30)
        break
    if scenario == "secret":
        print(
            json.dumps(
                {
                    "type": "message_end",
                    "message": {"content": os.environ.get("FACTORY_PROVIDER_KEY")},
                }
            ),
            flush=True,
        )
        print(json.dumps({"type": "agent_settled"}), flush=True)
        continue
    print(
        json.dumps({"type": "message_end", "message": {"role": "assistant"}}),
        flush=True,
    )
    print(json.dumps({"type": "agent_settled"}), flush=True)
'''


def make_profile() -> SpecialistProfile:
    return SpecialistProfile(
        name="implementer",
        role="Implementation",
        model="configured-model",
        provider="configured-provider",
        provider_api_key_env="FACTORY_PROVIDER_KEY",
        skills=(),
    )


def make_supervisor(
    tmp_path: Path,
    scenario: str,
    *,
    event_handler: Any = None,
) -> PiRpcSupervisor:
    return PiRpcSupervisor(
        make_profile(),
        tmp_path,
        command=(sys.executable, "-u", "-c", FAKE_PI, scenario),
        event_handler=event_handler,
    )


def test_prompt_is_complete_only_after_agent_settled(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("FACTORY_PROVIDER_KEY", "pi-test-secret")
    observed: list[dict[str, Any]] = []
    supervisor = make_supervisor(tmp_path, "settled", event_handler=observed.append)

    result = supervisor.run_prompt("Implement the requested change", timeout=2)

    assert result.settled is True
    assert "agent_settled" in [event.get("type") for event in result.events]
    assert observed == list(result.events)
    supervisor.close()


def test_rpc_command_uses_model_and_never_embeds_api_key_in_argv(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("FACTORY_PROVIDER_KEY", "pi-test-secret")
    monkeypatch.setenv("UNRELATED_HOST_SECRET", "host-only")
    supervisor = PiRpcSupervisor(make_profile(), tmp_path, executable="pi")

    command = supervisor.command
    environment = supervisor.child_environment()

    assert command[:4] == [
        "pi",
        "--mode",
        "rpc",
        "--no-session",
    ]
    assert "--provider" in command
    assert "configured-provider" in command
    assert "--model" in command
    assert "configured-model" in command
    assert "--no-extensions" in command
    assert "--no-context-files" in command
    assert "pi-test-secret" not in command
    assert environment["FACTORY_PROVIDER_KEY"] == "pi-test-secret"
    assert "UNRELATED_HOST_SECRET" not in environment


def test_api_key_in_pi_events_is_redacted_before_callbacks_and_results(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    secret = "pi-test-secret"
    monkeypatch.setenv("FACTORY_PROVIDER_KEY", secret)
    observed: list[dict[str, Any]] = []
    supervisor = make_supervisor(tmp_path, "secret", event_handler=observed.append)

    result = supervisor.run_prompt("Print a secret", timeout=2)

    serialized = json.dumps(result.events) + json.dumps(observed)
    assert secret not in serialized
    assert "[REDACTED]" in serialized
    supervisor.close()


def test_malformed_jsonl_record_fails_worker_safely(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("FACTORY_PROVIDER_KEY", "pi-test-secret")
    supervisor = make_supervisor(tmp_path, "malformed")

    with pytest.raises(PiProtocolError, match="invalid JSONL"):
        supervisor.run_prompt("Run", timeout=2)

    assert supervisor.process is None


def test_process_exit_before_agent_settled_is_a_worker_failure(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("FACTORY_PROVIDER_KEY", "pi-test-secret")
    supervisor = make_supervisor(tmp_path, "exit")

    with pytest.raises(PiProcessExited, match="before agent_settled"):
        supervisor.run_prompt("Run", timeout=2)


def test_accepted_but_unsettled_prompt_times_out_without_reporting_completion(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("FACTORY_PROVIDER_KEY", "pi-test-secret")
    supervisor = make_supervisor(tmp_path, "unsettled")

    with pytest.raises(PiRunTimeout):
        supervisor.run_prompt("Run", timeout=0.1)

    assert supervisor.process is None


def test_prompt_rejection_is_not_mistaken_for_completion(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("FACTORY_PROVIDER_KEY", "pi-test-secret")
    supervisor = make_supervisor(tmp_path, "rejected")

    with pytest.raises(PiCommandRejected, match="prompt rejected"):
        supervisor.run_prompt("Run", timeout=2)

    assert supervisor.process is None
