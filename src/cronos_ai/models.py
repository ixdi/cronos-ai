"""Validated configuration and domain models for factory execution."""

import hashlib
import re
from datetime import UTC, datetime
from enum import StrEnum
from pathlib import Path
from typing import Annotated, Self

from pydantic import (
    AnyHttpUrl,
    BaseModel,
    ConfigDict,
    Field,
    StringConstraints,
    field_validator,
    model_validator,
)

NonEmptyString = Annotated[
    str,
    StringConstraints(strip_whitespace=True, min_length=1),
]
ChangeName = Annotated[
    str,
    StringConstraints(
        strip_whitespace=True,
        min_length=1,
        pattern=r"^[a-z0-9]+(?:-[a-z0-9]+)*$",
    ),
]
PlanHash = Annotated[str, StringConstraints(pattern=r"^[a-f0-9]{64}$")]
SkillName = Annotated[
    str,
    StringConstraints(
        strip_whitespace=True,
        min_length=1,
        pattern=r"^[a-z0-9]+(?:-[a-z0-9]+)*$",
    ),
]
MCPToolName = Annotated[
    str,
    StringConstraints(
        strip_whitespace=True,
        min_length=1,
        max_length=64,
        pattern=r"^[A-Za-z0-9_.-]+$",
    ),
]
EnvironmentVariable = Annotated[
    str,
    StringConstraints(pattern=r"^[A-Z_][A-Z0-9_]*$"),
]


class ValidatedModel(BaseModel):
    """Base class that rejects unknown fields and prevents invalid mutation."""

    model_config = ConfigDict(extra="forbid", frozen=True)


class RequestSource(StrEnum):
    USER = "user"
    MONITORING = "monitoring"


class TriageOutcome(StrEnum):
    ACTIONABLE = "actionable"
    SPECS_REQUIRED = "specifications-required"
    CLARIFICATION_REQUIRED = "clarification-required"
    PARKED = "parked"


class PlanRisk(StrEnum):
    SUBSTANTIAL = "substantial"
    SECURITY = "security"
    AUTHENTICATION = "authentication"
    DESTRUCTIVE_MIGRATION = "destructive-migration"
    PRODUCTION_INFRASTRUCTURE = "production-infrastructure"
    DEPLOYMENT = "deployment"


class ApprovalDecision(StrEnum):
    APPROVED = "approved"
    REJECTED = "rejected"


class TaskState(StrEnum):
    QUEUED = "queued"
    READY = "ready"
    RUNNING = "running"
    INTEGRATING = "integrating"
    WAITING_FOR_HUMAN = "waiting for human"
    BLOCKED = "blocked"
    REVIEW = "review"
    DONE = "done"
    FAILED = "failed"


class WorkerStatus(StrEnum):
    IDLE = "idle"
    WORKING = "working"
    BLOCKED = "blocked"
    DONE = "done"


class AttemptStatus(StrEnum):
    RUNNING = "running"
    SUCCEEDED = "succeeded"
    FAILED = "failed"
    INTERRUPTED = "interrupted"


class IntegrationOutcome(StrEnum):
    SUCCEEDED = "succeeded"
    FAILED = "failed"
    INCONCLUSIVE = "inconclusive"


class ControllerLifecycle(StrEnum):
    RUNNING = "running"
    STOPPED = "stopped"


class ControlActionType(StrEnum):
    APPROVE_PLAN = "approve-plan"
    CLARIFY_REQUEST = "clarify-request"
    RETRY_TASK = "retry-task"
    RESOLVE_CONFLICT = "resolve-conflict"
    APPROVE_REVIEW = "approve-review"
    REJECT_REVIEW = "reject-review"
    RESUME_REQUEST = "resume-request"


class ControlActionStatus(StrEnum):
    PENDING = "pending"
    PROCESSING = "processing"
    DONE = "done"
    FAILED = "failed"


class ReviewState(StrEnum):
    READY = "ready"
    BLOCKED = "blocked"
    APPROVED = "approved"
    REJECTED = "rejected"


class FactoryConfig(ValidatedModel):
    """Validated local-controller limits and state location."""

    state_dir: Path
    max_concurrency: int = Field(default=3, gt=0)
    max_attempts: int = Field(default=3, ge=1)


class WorkRequest(ValidatedModel):
    """A normalized request and its explicit target repository."""

    request_id: NonEmptyString
    description: NonEmptyString
    repo_path: Path
    source: RequestSource = RequestSource.USER
    triage_outcome: TriageOutcome | None = None


class PlanTaskKind(StrEnum):
    PLANNING = "planning"
    APPROVAL = "approval"
    IMPLEMENTATION = "implementation"
    VERIFICATION = "verification"


class PlanTask(ValidatedModel):
    """A task, execution stage, and dependency edge from an OpenSpec plan."""

    task_id: NonEmptyString
    description: NonEmptyString
    depends_on: tuple[NonEmptyString, ...] = ()
    kind: PlanTaskKind = PlanTaskKind.IMPLEMENTATION

    @model_validator(mode="after")
    def validate_dependencies(self) -> Self:
        if self.task_id in self.depends_on:
            raise ValueError("a task cannot depend on itself")
        if len(self.depends_on) != len(set(self.depends_on)):
            raise ValueError("task dependencies must be unique")
        return self


class OpenSpecPlan(ValidatedModel):
    """An OpenSpec change and its validated dependency graph."""

    change_name: ChangeName
    tasks: tuple[PlanTask, ...] = Field(min_length=1)

    @model_validator(mode="after")
    def validate_task_graph(self) -> Self:
        task_ids = [task.task_id for task in self.tasks]
        if len(task_ids) != len(set(task_ids)):
            raise ValueError("plan task identifiers must be unique")

        known_ids = set(task_ids)
        for task in self.tasks:
            missing = set(task.depends_on) - known_ids
            if missing:
                raise ValueError(
                    f"task {task.task_id!r} has unknown dependencies: "
                    f"{', '.join(sorted(missing))}"
                )

        dependencies = {task.task_id: task.depends_on for task in self.tasks}
        visited: set[str] = set()
        visiting: set[str] = set()

        def visit(task_id: str) -> None:
            if task_id in visiting:
                raise ValueError("plan task dependencies must not contain cycles")
            if task_id in visited:
                return
            visiting.add(task_id)
            for dependency in dependencies[task_id]:
                visit(dependency)
            visiting.remove(task_id)
            visited.add(task_id)

        for task_id in task_ids:
            visit(task_id)
        return self


class TriageResult(ValidatedModel):
    """A recorded triage outcome and its permitted next workflow stage."""

    request: WorkRequest
    outcome: TriageOutcome
    rationale: NonEmptyString
    questions: tuple[NonEmptyString, ...] = ()
    risks: tuple[PlanRisk, ...] = ()

    @model_validator(mode="after")
    def validate_questions(self) -> Self:
        if self.outcome is TriageOutcome.CLARIFICATION_REQUIRED and not self.questions:
            raise ValueError("clarification requires at least one blocking question")
        if self.outcome is not TriageOutcome.CLARIFICATION_REQUIRED and self.questions:
            raise ValueError("blocking questions are only valid for clarification")
        if self.request.triage_outcome is not self.outcome:
            raise ValueError("request triage outcome must match the result")
        if len(self.risks) != len(set(self.risks)):
            raise ValueError("triage risks must not contain duplicates")
        return self

    @property
    def requires_approval(self) -> bool:
        """Whether plan approval is required before implementation."""
        return self.outcome is TriageOutcome.SPECS_REQUIRED or bool(self.risks)

    @property
    def can_plan(self) -> bool:
        """Whether triage allows concise or detailed OpenSpec planning."""
        return self.outcome in (
            TriageOutcome.ACTIONABLE,
            TriageOutcome.SPECS_REQUIRED,
        )

    @property
    def can_dispatch_directly(self) -> bool:
        """Whether the request can bypass a plan-approval gate."""
        return self.outcome is TriageOutcome.ACTIONABLE and not self.requires_approval


class PlanApproval(ValidatedModel):
    """An auditable approval decision bound to one exact plan version."""

    change_name: ChangeName
    plan_hash: PlanHash
    decision: ApprovalDecision
    reviewer: NonEmptyString
    rationale: NonEmptyString | None = None
    recorded_at: datetime = Field(default_factory=lambda: datetime.now(UTC))

    @model_validator(mode="after")
    def validate_decision(self) -> Self:
        if self.recorded_at.tzinfo is None or self.recorded_at.utcoffset() is None:
            raise ValueError("approval timestamp must be timezone-aware")
        if self.decision is ApprovalDecision.REJECTED and self.rationale is None:
            raise ValueError("rejected approvals require a reason")
        return self


class TaskRecord(ValidatedModel):
    """Current durable lifecycle state for an OpenSpec task."""

    task_id: NonEmptyString
    state: TaskState = TaskState.QUEUED
    reason: NonEmptyString | None = None
    attempt_count: int = Field(default=0, ge=0)
    worker_id: NonEmptyString | None = None

    @model_validator(mode="after")
    def require_reason_for_terminal_attention(self) -> Self:
        if self.state in (TaskState.BLOCKED, TaskState.FAILED) and self.reason is None:
            raise ValueError("blocked and failed tasks require a reason")
        if self.state is TaskState.RUNNING and self.worker_id is None:
            raise ValueError("running tasks require an assigned worker")
        return self


class ActivityEvent(ValidatedModel):
    """A bounded, safe-to-display progress or lifecycle event."""

    event_id: NonEmptyString
    run_id: NonEmptyString
    task_id: NonEmptyString | None = None
    occurred_at: datetime
    category: Annotated[
        str,
        StringConstraints(strip_whitespace=True, min_length=1, max_length=64),
    ]
    summary: Annotated[
        str,
        StringConstraints(strip_whitespace=True, min_length=1, max_length=512),
    ]

    @field_validator("occurred_at")
    @classmethod
    def normalize_timestamp(cls, value: datetime) -> datetime:
        if value.tzinfo is None or value.utcoffset() is None:
            raise ValueError("activity timestamps must be timezone-aware")
        return value.astimezone(UTC)


class MCPServerDefinition(ValidatedModel):
    """Explicit local MCP stdio server command and its approved tool catalog."""

    server_id: Annotated[
        str,
        StringConstraints(
            strip_whitespace=True,
            min_length=1,
            pattern=r"^[a-z][a-z0-9_-]{0,31}$",
        ),
    ]
    command: NonEmptyString
    arguments: tuple[str, ...] = ()
    tools: tuple[MCPToolName, ...] = Field(min_length=1)

    @model_validator(mode="after")
    def validate_catalog(self) -> Self:
        if "\0" in self.command or any("\0" in argument for argument in self.arguments):
            raise ValueError("MCP server command arguments must not contain NUL")
        if len(self.tools) != len(set(self.tools)):
            raise ValueError("MCP tool names must be unique within a server")
        return self


class SpecialistProfile(ValidatedModel):
    """Explicitly allowlisted runtime resources for a specialist agent."""

    name: NonEmptyString
    role: NonEmptyString
    model: NonEmptyString
    provider: NonEmptyString
    provider_api_key_env: EnvironmentVariable
    skills: tuple[SkillName, ...] = ()
    mcp_tools: tuple[NonEmptyString, ...] = ()
    network_allowlist: tuple[AnyHttpUrl, ...] = ()

    @model_validator(mode="after")
    def require_unique_allowlists(self) -> Self:
        for destination in self.network_allowlist:
            if (
                destination.scheme != "https"
                or destination.port not in (None, 443)
                or destination.path not in ("", "/")
                or destination.query is not None
                or destination.fragment is not None
                or destination.username is not None
                or destination.password is not None
            ):
                raise ValueError(
                    "network allowlist entries must be HTTPS origins "
                    "without credentials"
                )
        if len(self.skills) != len(set(self.skills)):
            raise ValueError("skills must not contain duplicates")
        if len(self.mcp_tools) != len(set(self.mcp_tools)):
            raise ValueError("MCP tools must not contain duplicates")
        if len(self.network_allowlist) != len(set(self.network_allowlist)):
            raise ValueError("network destinations must not contain duplicates")
        return self


class WorkerSlot(ValidatedModel):
    """A reusable Herdr pane and its current factory assignment."""

    worker_id: NonEmptyString
    session_id: NonEmptyString
    workspace_id: NonEmptyString | None = None
    pane_id: NonEmptyString
    status: WorkerStatus = WorkerStatus.IDLE
    active_task_id: NonEmptyString | None = None

    @model_validator(mode="after")
    def require_task_for_active_worker(self) -> Self:
        if self.status in (WorkerStatus.WORKING, WorkerStatus.BLOCKED):
            if self.active_task_id is None:
                raise ValueError("working or blocked workers require an active task")
        return self


class Attempt(ValidatedModel):
    """A single bounded task execution attempt."""

    task_id: NonEmptyString
    attempt_number: int = Field(ge=1)
    status: AttemptStatus
    started_at: datetime = Field(default_factory=lambda: datetime.now(UTC))
    finished_at: datetime | None = None
    worker_id: NonEmptyString | None = None
    reason: NonEmptyString | None = None
    transient: bool = False

    @model_validator(mode="after")
    def validate_lifecycle(self) -> Self:
        if self.started_at.tzinfo is None or self.started_at.utcoffset() is None:
            raise ValueError("attempt timestamps must be timezone-aware")
        if self.finished_at is not None:
            if self.finished_at.tzinfo is None or self.finished_at.utcoffset() is None:
                raise ValueError("attempt timestamps must be timezone-aware")
            if self.finished_at < self.started_at:
                raise ValueError("attempt cannot finish before it starts")

        if self.status is AttemptStatus.RUNNING and self.finished_at is not None:
            raise ValueError("running attempts cannot have a finish time")
        if self.status is not AttemptStatus.RUNNING and self.finished_at is None:
            raise ValueError("finished attempts require a finish time")
        if self.status in (AttemptStatus.FAILED, AttemptStatus.INTERRUPTED):
            if self.reason is None:
                raise ValueError("failed or interrupted attempts require a reason")
        if self.status is not AttemptStatus.FAILED and self.transient:
            raise ValueError("only failed attempts can be transient")
        return self


class ControllerStatus(ValidatedModel):
    """Persisted lifecycle and heartbeat information for the controller."""

    state: ControllerLifecycle
    pid: int = Field(gt=0)
    started_at: datetime
    heartbeat_at: datetime
    stopped_at: datetime | None = None

    @model_validator(mode="after")
    def validate_timestamps(self) -> Self:
        timestamps = (self.started_at, self.heartbeat_at)
        if any(
            timestamp.tzinfo is None or timestamp.utcoffset() is None
            for timestamp in timestamps
        ):
            raise ValueError("controller timestamps must be timezone-aware")
        if self.stopped_at is not None:
            if self.stopped_at.tzinfo is None or self.stopped_at.utcoffset() is None:
                raise ValueError("controller timestamps must be timezone-aware")
            if self.state is not ControllerLifecycle.STOPPED:
                raise ValueError("running controllers cannot have a stopped timestamp")
        if self.state is ControllerLifecycle.RUNNING and self.stopped_at is not None:
            raise ValueError("running controllers cannot have a stopped timestamp")
        if self.state is ControllerLifecycle.STOPPED and self.stopped_at is None:
            raise ValueError("stopped controllers require a stopped timestamp")
        if self.heartbeat_at < self.started_at:
            raise ValueError("controller heartbeat cannot predate startup")
        return self


class RunContext(ValidatedModel):
    """Durable branch, worktree, and approval references for a factory run."""

    run_id: NonEmptyString
    branch_name: NonEmptyString
    run_worktree_path: Path
    starting_commit: NonEmptyString | None = None
    task_worktree_root: Path | None = None
    plan_hash: PlanHash | None = None
    approval_required: bool = False
    triage: TriageResult | None = None

    @model_validator(mode="after")
    def validate_context(self) -> Self:
        if not self.run_worktree_path.is_absolute():
            raise ValueError("run worktree path must be absolute")
        if self.starting_commit is not None and not re.fullmatch(
            r"(?:[a-f0-9]{40}|[a-f0-9]{64})", self.starting_commit
        ):
            raise ValueError("starting commit must be a full Git object ID")
        if (
            self.task_worktree_root is not None
            and not self.task_worktree_root.is_absolute()
        ):
            raise ValueError("task worktree root must be absolute")
        if self.approval_required and self.plan_hash is None:
            raise ValueError("approval-required runs need a plan fingerprint")
        if self.triage is not None and self.triage.requires_approval:
            if not self.approval_required:
                raise ValueError("high-impact triage must require plan approval")
        return self


class ControlAction(ValidatedModel):
    """A durable request for a human-controlled factory action."""

    action_id: NonEmptyString
    action_type: ControlActionType
    target_id: NonEmptyString
    run_id: NonEmptyString | None = None
    actor: NonEmptyString = "local-operator"
    payload: dict[NonEmptyString, NonEmptyString] = Field(default_factory=dict)
    created_at: datetime = Field(default_factory=lambda: datetime.now(UTC))

    @model_validator(mode="after")
    def validate_action_payload(self) -> Self:
        run_scoped_actions = {
            ControlActionType.APPROVE_PLAN,
            ControlActionType.RETRY_TASK,
            ControlActionType.RESOLVE_CONFLICT,
            ControlActionType.APPROVE_REVIEW,
            ControlActionType.REJECT_REVIEW,
        }
        if self.action_type in run_scoped_actions and self.run_id is None:
            raise ValueError("this action requires a run identifier")
        if self.action_type is ControlActionType.APPROVE_PLAN:
            if not re.fullmatch(r"[a-f0-9]{64}", self.payload.get("plan_hash", "")):
                raise ValueError("approve-plan requires a valid plan hash")
        if (
            self.action_type
            in (ControlActionType.APPROVE_REVIEW, ControlActionType.REJECT_REVIEW)
            and self.target_id == "run"
            and not re.fullmatch(r"[a-f0-9]{64}", self.payload.get("review_hash", ""))
        ):
            raise ValueError("run-level review actions require a review hash")
        if self.action_type is ControlActionType.CLARIFY_REQUEST:
            if not self.payload.get("answer"):
                raise ValueError("clarify-request requires an answer")
        if self.action_type is ControlActionType.RESOLVE_CONFLICT:
            if not self.payload.get("resolution"):
                raise ValueError("resolve-conflict requires a resolution")
        if self.action_type in (
            ControlActionType.APPROVE_PLAN,
            ControlActionType.APPROVE_REVIEW,
            ControlActionType.REJECT_REVIEW,
        ) and not self.payload.get("reviewer"):
            raise ValueError("approval and rejection actions require a reviewer")
        if self.created_at.tzinfo is None or self.created_at.utcoffset() is None:
            raise ValueError("action timestamp must be timezone-aware")
        return self


class ControlActionRecord(ValidatedModel):
    """Durable lifecycle and audit status for a submitted human action."""

    action: ControlAction
    status: ControlActionStatus = ControlActionStatus.PENDING
    error: NonEmptyString | None = None
    processed_at: datetime | None = None

    @model_validator(mode="after")
    def validate_processing_result(self) -> Self:
        if self.status in (ControlActionStatus.DONE, ControlActionStatus.FAILED):
            if self.processed_at is None:
                raise ValueError("finished actions require a processing timestamp")
        if self.status is ControlActionStatus.FAILED and self.error is None:
            raise ValueError("failed actions require a reason")
        if self.processed_at is not None and (
            self.processed_at.tzinfo is None or self.processed_at.utcoffset() is None
        ):
            raise ValueError("action processing timestamps must be timezone-aware")
        return self


class AttentionItem(ValidatedModel):
    """An actionable task or request shown in the human attention queue."""

    item_id: NonEmptyString
    run_id: NonEmptyString | None = None
    target_id: NonEmptyString | None = None
    task_id: NonEmptyString | None = None
    request_id: NonEmptyString | None = None
    state: TaskState | TriageOutcome
    reason: NonEmptyString
    latest_result: NonEmptyString | None = None
    suggested_data: dict[NonEmptyString, NonEmptyString] = Field(default_factory=dict)
    available_actions: tuple[ControlActionType, ...] = ()
    recommended_action: NonEmptyString | None = None

    @model_validator(mode="after")
    def require_next_step(self) -> Self:
        if not self.available_actions and self.recommended_action is None:
            raise ValueError("attention items require an actionable next step")
        return self


class VerificationCheck(ValidatedModel):
    """A command or automated check result included in review evidence."""

    name: NonEmptyString
    command: NonEmptyString
    passed: bool
    summary: NonEmptyString


class UserScenarioCheck(ValidatedModel):
    """A user-perspective scenario and its observed verification result."""

    scenario: NonEmptyString
    passed: bool
    evidence: NonEmptyString


class ReviewPacket(ValidatedModel):
    """Immutable review evidence with a separately recorded human decision."""

    review_id: NonEmptyString
    run_id: NonEmptyString
    base_commit: NonEmptyString
    head_commit: NonEmptyString
    diff_text: str
    diff_hash: PlanHash
    review_hash: PlanHash
    review_summary: NonEmptyString
    review_findings: tuple[NonEmptyString, ...] = ()
    verification_checks: tuple[VerificationCheck, ...] = Field(min_length=1)
    user_scenarios: tuple[UserScenarioCheck, ...] = Field(min_length=1)
    blocking_findings: tuple[NonEmptyString, ...] = ()
    state: ReviewState = ReviewState.READY
    reviewer: NonEmptyString | None = None
    rationale: NonEmptyString | None = None
    created_at: datetime = Field(default_factory=lambda: datetime.now(UTC))
    decided_at: datetime | None = None

    @model_validator(mode="after")
    def validate_review_lifecycle(self) -> Self:
        if hashlib.sha256(self.diff_text.encode("utf-8")).hexdigest() != self.diff_hash:
            raise ValueError("review diff hash does not match the included diff")
        for timestamp in (self.created_at, self.decided_at):
            if timestamp is not None and (
                timestamp.tzinfo is None or timestamp.utcoffset() is None
            ):
                raise ValueError("review timestamps must be timezone-aware")
        ready_evidence = (
            all(check.passed for check in self.verification_checks)
            and all(scenario.passed for scenario in self.user_scenarios)
            and not self.blocking_findings
        )
        if (
            self.state in (ReviewState.READY, ReviewState.APPROVED)
            and not ready_evidence
        ):
            raise ValueError("review approval requires passing verification evidence")
        if self.state is ReviewState.BLOCKED and ready_evidence:
            raise ValueError(
                "blocked review packets require a failing check or finding"
            )
        if self.state in (ReviewState.APPROVED, ReviewState.REJECTED):
            if self.reviewer is None or self.decided_at is None:
                raise ValueError("review decisions require a reviewer and timestamp")
        if self.state is ReviewState.REJECTED and self.rationale is None:
            raise ValueError("rejected reviews require a rationale")
        if self.decided_at is not None and self.decided_at < self.created_at:
            raise ValueError("review decision cannot predate packet creation")
        return self


class WebhookAlertPayload(ValidatedModel):
    """Strict provider-neutral alert content with no executable commands."""

    repository: NonEmptyString
    description: NonEmptyString

    @model_validator(mode="after")
    def validate_repository(self) -> Self:
        if not Path(self.repository).is_absolute():
            raise ValueError("webhook repository must be an absolute path")
        if "\0" in self.repository:
            raise ValueError("webhook repository path contains a null byte")
        return self


class WebhookEvent(ValidatedModel):
    """Authenticated alert event persisted for ordinary triage processing."""

    source_id: NonEmptyString
    event_id: NonEmptyString
    timestamp: datetime
    body_sha256: PlanHash
    payload: WebhookAlertPayload
    received_at: datetime

    @model_validator(mode="after")
    def validate_event(self) -> Self:
        if not re.fullmatch(r"[a-z][a-z0-9_-]{0,31}", self.source_id):
            raise ValueError("webhook source ID has an invalid format")
        if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._:-]{0,127}", self.event_id):
            raise ValueError("webhook event ID has an invalid format")
        for value in (self.timestamp, self.received_at):
            if value.tzinfo is None or value.utcoffset() is None:
                raise ValueError("webhook timestamps must be timezone-aware")
        return self


class WebhookReceipt(StrEnum):
    ACCEPTED = "accepted"
    DUPLICATE = "duplicate"
    CONFLICT = "conflict"


class DeliveryStatus(StrEnum):
    PENDING = "pending"
    RUNNING = "running"
    SUCCEEDED = "succeeded"
    FAILED = "failed"
    INCONCLUSIVE = "inconclusive"


class CICDRequest(ValidatedModel):
    """Approved immutable revision sent to an injected CI/CD adapter."""

    run_id: NonEmptyString
    repository: Path
    branch_name: NonEmptyString
    revision: NonEmptyString
    review_hash: PlanHash
    idempotency_key: PlanHash


class CICDResult(ValidatedModel):
    """Provider-neutral dispatch or workflow status response."""

    external_id: NonEmptyString | None = None
    status: DeliveryStatus
    summary: NonEmptyString


class DeliveryRecord(ValidatedModel):
    """Durable CI/CD attempt bound to one approved review packet."""

    request: CICDRequest
    status: DeliveryStatus
    external_id: NonEmptyString | None = None
    summary: NonEmptyString
    created_at: datetime = Field(default_factory=lambda: datetime.now(UTC))
    updated_at: datetime = Field(default_factory=lambda: datetime.now(UTC))

    @model_validator(mode="after")
    def validate_delivery_record(self) -> Self:
        if self.status in (DeliveryStatus.RUNNING, DeliveryStatus.SUCCEEDED):
            if self.external_id is None:
                raise ValueError(
                    "active or successful CI/CD runs require an external ID"
                )
        if self.updated_at < self.created_at:
            raise ValueError("delivery update timestamp cannot predate dispatch")
        for timestamp in (self.created_at, self.updated_at):
            if timestamp.tzinfo is None or timestamp.utcoffset() is None:
                raise ValueError("delivery timestamps must be timezone-aware")
        return self


class IntegrationResult(ValidatedModel):
    """An authoritative result reported by a configured integration."""

    integration: NonEmptyString
    outcome: IntegrationOutcome
    external_id: NonEmptyString | None = None
    message: NonEmptyString | None = None

    @model_validator(mode="after")
    def require_evidence(self) -> Self:
        if self.outcome is IntegrationOutcome.SUCCEEDED and self.external_id is None:
            raise ValueError(
                "successful integration results require an external reference"
            )
        if self.outcome is not IntegrationOutcome.SUCCEEDED and self.message is None:
            raise ValueError("unsuccessful integration results require a reason")
        return self
