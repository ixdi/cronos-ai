"""Command-line interface for the software factory."""

import argparse
import json
from collections.abc import Sequence
from pathlib import Path
from uuid import uuid4

from pydantic import ValidationError

from cronos_ai import __version__
from cronos_ai.attention import HumanAttentionQueue
from cronos_ai.controller import (
    LocalController,
    StateMigrationError,
    default_state_dir,
)
from cronos_ai.intake import IntakeError, intake_request
from cronos_ai.models import (
    ControlAction,
    ControlActionType,
    UserScenarioCheck,
    VerificationCheck,
)
from cronos_ai.repository_init import (
    RepositoryInitializationError,
    initialize_repository,
)
from cronos_ai.review import ReviewCoordinator, ReviewError
from cronos_ai.storage import ControllerLockError, FactoryStore


def _add_state_dir(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--state-dir", type=Path)


def _parse_action_payload(
    parser: argparse.ArgumentParser,
    values: list[str],
) -> dict[str, str]:
    payload: dict[str, str] = {}
    for value in values:
        key, separator, content = value.partition("=")
        if not separator or not key.strip() or not content.strip():
            parser.error("--data values must use non-empty key=value pairs")
        if key in payload:
            parser.error(f"duplicate --data key: {key}")
        payload[key] = content
    return payload


def main(argv: Sequence[str] | None = None) -> None:
    """Parse and run CLI commands."""
    parser = argparse.ArgumentParser(prog="cronos-ai")
    parser.add_argument(
        "--version",
        action="version",
        version=f"%(prog)s {__version__}",
    )
    commands = parser.add_subparsers(dest="command")

    init_parser = commands.add_parser("init", help="initialize a Git repository")
    init_parser.add_argument("--repo", required=True, type=Path)

    run_parser = commands.add_parser("run", help="queue a request for the controller")
    run_parser.add_argument("--repo", required=True, type=Path)
    run_parser.add_argument("--request", required=True)
    _add_state_dir(run_parser)

    status_parser = commands.add_parser("status", help="inspect controller and queue")
    _add_state_dir(status_parser)

    attention_parser = commands.add_parser(
        "attention", help="list work that requires a human decision"
    )
    attention_parser.add_argument("--run", help="limit the queue to one run")
    attention_parser.add_argument(
        "--json", action="store_true", help="print machine-readable JSON"
    )
    _add_state_dir(attention_parser)

    review_parser = commands.add_parser(
        "review", help="inspect integrated code and verification evidence"
    )
    review_parser.add_argument("--run", required=True)
    review_parser.add_argument(
        "--evidence",
        type=Path,
        help="JSON file with code-review, automated-check, and user-scenario results",
    )
    review_parser.add_argument(
        "--json", action="store_true", help="print machine-readable JSON"
    )
    _add_state_dir(review_parser)

    actions_parser = commands.add_parser(
        "actions", help="inspect the durable human-action audit log"
    )
    actions_parser.add_argument("--run", help="filter actions by run identifier")
    actions_parser.add_argument(
        "--json", action="store_true", help="print machine-readable JSON"
    )
    _add_state_dir(actions_parser)

    action_parser = commands.add_parser("action", help="queue a human action")
    action_parser.add_argument(
        "--action",
        required=True,
        choices=[action.value for action in ControlActionType],
    )
    action_parser.add_argument("--target", required=True)
    action_parser.add_argument("--run", help="run identifier for run-scoped actions")
    action_parser.add_argument("--actor", default="local-operator")
    action_parser.add_argument("--data", action="append", default=[])
    _add_state_dir(action_parser)

    controller_parser = commands.add_parser("controller", help="manage the controller")
    controller_commands = controller_parser.add_subparsers(
        dest="controller_command",
        required=True,
    )
    start_parser = controller_commands.add_parser(
        "start",
        help="run the local controller until interrupted",
    )
    _add_state_dir(start_parser)

    args = parser.parse_args(argv)
    if hasattr(args, "state_dir") and args.state_dir is None:
        try:
            args.state_dir = default_state_dir()
        except StateMigrationError as error:
            parser.exit(1, f"error: {error}\n")
    if args.command == "init":
        try:
            repository = initialize_repository(args.repo)
        except RepositoryInitializationError as error:
            parser.exit(1, f"error: {error}\n")
        print(f"OpenSpec is ready in {repository}")
    elif args.command == "run":
        try:
            request = intake_request(args.repo, args.request)
        except IntakeError as error:
            parser.exit(1, f"error: {error}\n")
        with FactoryStore(args.state_dir / "factory.sqlite3") as store:
            store.enqueue_request(request)
        print(f"Queued request {request.request_id}")
    elif args.command == "status":
        with FactoryStore(args.state_dir / "factory.sqlite3") as store:
            controller_status = store.get_controller_status()
            requests = store.list_queued_requests()
            actions = store.list_pending_actions()
            attention_items = HumanAttentionQueue(store).list_items()
        controller_label = (
            "not running"
            if controller_status is None
            else controller_status.state.value
        )
        print(f"Controller: {controller_label}")
        print(f"Queued requests: {len(requests)}")
        print(f"Pending human actions: {len(actions)}")
        print(f"Items needing human attention: {len(attention_items)}")
    elif args.command == "attention":
        with FactoryStore(args.state_dir / "factory.sqlite3") as store:
            items = HumanAttentionQueue(store).list_items(args.run)
        if args.json:
            print(
                json.dumps([item.model_dump(mode="json") for item in items], indent=2)
            )
        elif not items:
            print("No work currently requires human attention.")
        else:
            for item in items:
                print(f"[{item.state.value}] {item.item_id}: {item.reason}")
                if item.latest_result:
                    print(f"  Latest result: {item.latest_result}")
                if item.recommended_action:
                    print(f"  Next step: {item.recommended_action}")
                for action_type in item.available_actions:
                    target_id = (
                        item.target_id
                        or item.task_id
                        or item.request_id
                        or item.item_id
                    )
                    parts = [
                        "cronos-ai action",
                        f"--action {action_type.value}",
                        f"--target {target_id}",
                    ]
                    if item.run_id:
                        parts.extend(("--run", item.run_id))
                    for key, value in item.suggested_data.items():
                        if value:
                            parts.extend(("--data", f"{key}={value}"))
                    print("  Next action: " + " ".join(parts))
    elif args.command == "review":
        if args.evidence is not None:
            try:
                evidence = json.loads(args.evidence.read_text(encoding="utf-8"))
                verification_checks = tuple(
                    VerificationCheck.model_validate(value)
                    for value in evidence["verification_checks"]
                )
                user_scenarios = tuple(
                    UserScenarioCheck.model_validate(value)
                    for value in evidence["user_scenarios"]
                )
            except (
                OSError,
                json.JSONDecodeError,
                KeyError,
                TypeError,
                ValidationError,
            ) as error:
                parser.exit(2, f"error: invalid review evidence: {error}\n")
            try:
                with FactoryStore(args.state_dir / "factory.sqlite3") as store:
                    prepared_packet = ReviewCoordinator(store).prepare_review(
                        args.run,
                        review_summary=evidence["review_summary"],
                        review_findings=tuple(evidence.get("review_findings", ())),
                        verification_checks=verification_checks,
                        user_scenarios=user_scenarios,
                        blocking_findings=tuple(evidence.get("blocking_findings", ())),
                    )
            except (KeyError, ReviewError) as error:
                parser.exit(1, f"error: could not prepare review: {error}\n")
            print(
                f"Review packet {prepared_packet.review_id}: "
                f"{prepared_packet.state.value} "
                f"(fingerprint {prepared_packet.review_hash})"
            )
            return
        with FactoryStore(args.state_dir / "factory.sqlite3") as store:
            packet = store.get_latest_review_packet(args.run)
        if packet is None:
            parser.exit(1, f"error: no review packet exists for run {args.run}\n")
        if args.json:
            print(json.dumps(packet.model_dump(mode="json"), indent=2))
        else:
            print(f"Review for run {packet.run_id}: {packet.state.value}")
            print(f"Reviewer: {packet.reviewer or 'pending'}")
            print(f"Review fingerprint: {packet.review_hash}")
            print(f"Diff fingerprint: {packet.diff_hash}")
            print(f"Summary: {packet.review_summary}")
            if packet.review_findings:
                print("Code review findings:")
                for finding in packet.review_findings:
                    print(f"  - {finding}")
            print("Automated verification:")
            for check in packet.verification_checks:
                outcome = "PASS" if check.passed else "FAIL"
                print(f"  [{outcome}] {check.name} ({check.command}): {check.summary}")
            print("User-perspective scenarios:")
            for scenario in packet.user_scenarios:
                outcome = "PASS" if scenario.passed else "FAIL"
                print(f"  [{outcome}] {scenario.scenario}: {scenario.evidence}")
            if packet.rationale:
                print(f"Decision rationale: {packet.rationale}")
            print("Integrated diff:")
            print(packet.diff_text)
    elif args.command == "actions":
        with FactoryStore(args.state_dir / "factory.sqlite3") as store:
            records = store.list_control_action_records()
        if args.run:
            records = [record for record in records if record.action.run_id == args.run]
        if args.json:
            print(
                json.dumps(
                    [record.model_dump(mode="json") for record in records],
                    indent=2,
                )
            )
        elif not records:
            print("No human actions have been recorded.")
        else:
            for record in records:
                action = record.action
                scope = f"run={action.run_id or '-'} target={action.target_id}"
                print(
                    f"[{record.status.value}] {action.action_id} "
                    f"{action.action_type.value} {scope} actor={action.actor}"
                )
                if record.error:
                    print(f"  Outcome: {record.error}")
    elif args.command == "action":
        try:
            action = ControlAction(
                action_id=str(uuid4()),
                action_type=ControlActionType(args.action),
                target_id=args.target,
                run_id=args.run,
                actor=args.actor,
                payload=_parse_action_payload(parser, args.data),
            )
        except (ValidationError, ValueError) as error:
            parser.exit(2, f"error: {error}\n")
        with FactoryStore(args.state_dir / "factory.sqlite3") as store:
            store.enqueue_control_action(action)
        print(f"Queued human action {action.action_id}")
    elif args.command == "controller" and args.controller_command == "start":
        try:
            LocalController(args.state_dir).run_forever()
        except ControllerLockError as error:
            parser.exit(1, f"error: {error}\n")
        except KeyboardInterrupt:
            print("Stopping Cronos AI controller.")
