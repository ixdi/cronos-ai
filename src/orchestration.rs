//! The Rust step loop that replaces the Pi orchestrator session: before each agent step a
//! one-shot planner proposes the agents and their prompts, Rust validates the proposal (or uses a
//! templated default plan), and the workflow engine runs the step.

use std::{path::Path, time::Duration};

use serde::Deserialize;

use crate::{
    agent_profiles::AgentProfileCatalog,
    integrations::ToolCommands,
    model::{
        AgentStatus, ApprovalDecision, ProjectId, RecoveryAction, Task, TaskId, TaskStatus,
        WorkflowStep,
    },
    workflow::{
        AgentAdapter, AgentPlan, WorkflowEngine, WorkflowError, agent_count_range, validate_agents,
        validate_profile_assignments,
    },
};

/// Findings recorded for the current attempt that a step's agents should act on.
const MAX_FINDINGS: usize = 10;

/// What a step's agents need to know beyond the task itself.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StepContext {
    /// `changes_requested` findings of the current attempt, as `agent: detail`.
    pub findings: Vec<String>,
    /// The human's note when they last sent this attempt back to this step.
    pub revision_note: Option<String>,
    /// Context a human added when they clarified the current attempt.
    pub clarification: Option<String>,
}

impl StepContext {
    pub fn for_task(task: &Task, step: WorkflowStep) -> Self {
        let mut findings = task
            .agent_activity
            .iter()
            .filter(|activity| {
                activity.attempt == task.attempt && activity.status == AgentStatus::ChangesRequested
            })
            .filter_map(|activity| {
                activity
                    .detail
                    .as_deref()
                    .map(|detail| format!("{}: {detail}", activity.agent_name))
            })
            .collect::<Vec<_>>();
        if findings.len() > MAX_FINDINGS {
            findings.drain(..findings.len() - MAX_FINDINGS);
        }
        let revision_note = task
            .approval_history
            .iter()
            .rev()
            .find(|record| {
                record.attempt == task.attempt
                    && record.decision == ApprovalDecision::Rejected
                    && record.gate.revision_step() == step.as_str()
            })
            .and_then(|record| record.note.clone());
        let clarification = task
            .recovery_history
            .iter()
            .rev()
            .find(|record| {
                record.attempt == task.attempt && record.action == RecoveryAction::Clarify
            })
            .and_then(|record| record.note.clone());
        Self {
            findings,
            revision_note,
            clarification,
        }
    }

    fn describe(&self) -> String {
        let mut text = String::new();
        if let Some(clarification) = &self.clarification {
            text.push_str(&format!(
                "\nClarification from the human for this attempt: {clarification}\n"
            ));
        }
        if let Some(note) = &self.revision_note {
            text.push_str(&format!("\nRevision requested by the human: {note}\n"));
        }
        if !self.findings.is_empty() {
            text.push_str("\nFindings to address from review, verification, checks, or merges:\n");
            for finding in &self.findings {
                text.push_str(&format!("- {finding}\n"));
            }
        }
        text
    }
}

fn task_summary(task: &Task) -> String {
    format!(
        "Task: {}\n{}OpenSpec change: {}\n",
        task.title,
        task.description
            .as_deref()
            .map(|description| format!("Description: {description}\n"))
            .unwrap_or_default(),
        task.openspec_change()
    )
}

fn step_role(step: WorkflowStep) -> &'static str {
    match step {
        WorkflowStep::SpecificationGeneration => {
            "write the OpenSpec change (proposal, specs, design if needed, tasks) for the task and commit it"
        }
        WorkflowStep::CodeGeneration => {
            "implement the approved OpenSpec change with tests and commit; several agents each get a separate part and their own worktree"
        }
        WorkflowStep::CodeReview => {
            "review the task branch against the OpenSpec change, read-only, from complementary angles"
        }
        WorkflowStep::Verification => {
            "verify the task branch by running its tests, linters, and audits, read-only"
        }
        WorkflowStep::Documentation => {
            "update the user and developer documentation; each agent owns different files"
        }
        _ => "",
    }
}

/// Instructions for the step planner: the task, the step's job and limits, the profiles allowed
/// in the step, the context to pass on, and the exact answer format.
pub fn step_planning_prompt(
    task: &Task,
    step: WorkflowStep,
    context: &StepContext,
    profiles: &AgentProfileCatalog,
) -> String {
    let (min, max) = agent_count_range(step);
    let count = if min == max {
        format!("exactly {min} agent")
    } else {
        format!("{min} to {max} agents")
    };
    let profiles = profiles
        .for_step(step)
        .map(|profile| format!("- {}: {}", profile.id, profile.description))
        .collect::<Vec<_>>();
    let profiles = if profiles.is_empty() {
        "(none; omit profile_id)".to_owned()
    } else {
        profiles.join("\n")
    };
    format!(
        "Plan the `{step}` step of a Cronos-AI task. Its agents {role}.\n\n{task}{context}\n\
         Read the worktree (the OpenSpec change under openspec/, the code, and the docs) to write \
         specific prompts. Use {count}; prefer one unless the work clearly splits into independent \
         parts. Cronos appends its own contract (result file, write rules) to every prompt, so \
         describe only the role and the concrete work, including every finding and note above that \
         the agent must act on.\n\n\
         Profiles allowed in this step:\n{profiles}\n\n\
         Answer with one fenced json block and nothing after it:\n\
         ```json\n\
         {{\"agents\": [{{\"name\": \"lowercase-name\", \"prompt\": \"what this agent does\", \"profile_id\": null}}]}}\n\
         ```",
        step = step.as_str(),
        role = step_role(step),
        task = task_summary(task),
        context = context.describe(),
    )
}

#[derive(Deserialize)]
struct RawStepPlan {
    agents: Vec<AgentPlan>,
}

/// The JSON object in the answer: the last fenced json block, or else the outermost braces.
fn plan_json(answer: &str) -> Option<&str> {
    if let Some(start) = answer.rfind("```json") {
        let body = &answer[start + "```json".len()..];
        return Some(body.split("```").next().unwrap_or(body).trim());
    }
    let start = answer.find('{')?;
    let end = answer.rfind('}')?;
    (start < end).then(|| &answer[start..=end])
}

/// Reads the planner's agents for `step` and checks them exactly as running the step would.
pub fn parse_step_plan(
    answer: &str,
    step: WorkflowStep,
    profiles: &AgentProfileCatalog,
) -> Result<Vec<AgentPlan>, String> {
    let json = plan_json(answer).ok_or("the step planner's answer contained no JSON plan")?;
    let plan: RawStepPlan = serde_json::from_str(json)
        .map_err(|error| format!("the step plan is not valid: {error}"))?;
    validate_agents(step, &plan.agents).map_err(|error| error.to_string())?;
    validate_profile_assignments(profiles, step, &plan.agents)
        .map_err(|error| error.to_string())?;
    Ok(plan.agents)
}

/// The plan used when the step planner fails: one agent per step with a templated prompt that
/// carries the task and its context.
pub fn default_plan(task: &Task, step: WorkflowStep, context: &StepContext) -> Vec<AgentPlan> {
    let (name, work) = match step {
        WorkflowStep::SpecificationGeneration => (
            "author",
            "Write the OpenSpec change for this task: read the README, the code, and openspec/specs/ first, then write a proposal, spec deltas with testable scenarios, a design if there are real decisions, and small tasks.",
        ),
        WorkflowStep::CodeGeneration => (
            "builder",
            "Implement the approved OpenSpec change test-first, covering every scenario, and address every finding and note below.",
        ),
        WorkflowStep::CodeReview => (
            "reviewer",
            "Review the task branch against the OpenSpec change for correctness, tests, security, and maintainability, and report only verified, concrete findings.",
        ),
        WorkflowStep::Verification => (
            "verifier",
            "Run the project's tests, linters, and audits, and confirm every scenario of the OpenSpec change is exercised and passes.",
        ),
        _ => (
            "docs",
            "Update the README and docs/ so they describe the behavior this change adds or modifies, with exact commands and defaults.",
        ),
    };
    vec![AgentPlan {
        name: name.to_owned(),
        prompt: format!("{work}\n\n{}{}", task_summary(task), context.describe()),
        profile_id: None,
    }]
}

/// Proposes the agents for a step; the answer is parsed and validated by the caller.
pub trait StepPlanner {
    fn plan(&self, task: &Task, step: WorkflowStep, prompt: &str) -> Result<String, String>;
}

/// The production step planner: a one-shot, read-only Pi run in the task's worktree.
pub struct PiStepPlanner {
    pub tools: ToolCommands,
    pub model: Option<String>,
    pub timeout: Duration,
}

impl StepPlanner for PiStepPlanner {
    fn plan(&self, task: &Task, _step: WorkflowStep, prompt: &str) -> Result<String, String> {
        let worktree = task
            .worktree
            .as_deref()
            .ok_or("the task has no worktree to read")?;
        self.tools
            .run_step_planner(
                Path::new(worktree),
                self.model.as_deref(),
                prompt,
                self.timeout,
            )
            .map_err(|error| error.to_string())
    }
}

/// Runs the task's agent steps one after another until it reaches a human approval, the local
/// merge, Error, or Completed, and returns the task as it stands then. Each step is planned by
/// `planner`; when that fails or proposes an invalid plan, the default plan runs instead and the
/// reason is recorded as `orchestrator` activity.
pub fn run_task<A: AgentAdapter, P: StepPlanner>(
    engine: &WorkflowEngine<A>,
    planner: &P,
    profiles: &AgentProfileCatalog,
    project_id: ProjectId,
    task_id: TaskId,
    progress: &mut dyn FnMut(&str),
) -> Result<Task, WorkflowError> {
    loop {
        let task = engine.manager().get_task(project_id, task_id)?;
        let Some(step) = task
            .current_step
            .as_deref()
            .and_then(WorkflowStep::parse)
            .filter(|step| task.status == TaskStatus::InProgress && step.runs_agents())
        else {
            return Ok(task);
        };
        let context = StepContext::for_task(&task, step);
        progress(&format!("{}: planning the agents", step.as_str()));
        let prompt = step_planning_prompt(&task, step, &context, profiles);
        let agents = match planner
            .plan(&task, step, &prompt)
            .and_then(|answer| parse_step_plan(&answer, step, profiles))
        {
            Ok(agents) => agents,
            Err(reason) => {
                engine.manager().record_agent_activity(
                    project_id,
                    task_id,
                    "orchestrator".to_owned(),
                    step.as_str().to_owned(),
                    AgentStatus::Completed,
                    Some(format!(
                        "Step planner unavailable, so the default plan ran: {reason}"
                    )),
                )?;
                default_plan(&task, step, &context)
            }
        };
        progress(&format!(
            "{}: {} {} running ({})",
            step.as_str(),
            agents.len(),
            if agents.len() == 1 { "agent" } else { "agents" },
            agents
                .iter()
                .map(|agent| agent.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
        engine.run_step(project_id, task_id, step, agents)?;
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{StepContext, default_plan, parse_step_plan, step_planning_prompt};
    use crate::{
        agent_profiles::{AgentProfile, AgentProfileCatalog},
        model::{
            AgentActivity, AgentStatus, ApprovalDecision, ApprovalGate, ApprovalRecord, Priority,
            ProjectId, RecoveryAction, RecoveryRecord, Task, TaskId, TaskStatus, WorkflowStep,
        },
    };

    fn task() -> Task {
        Task {
            id: TaskId::new(),
            project_id: ProjectId::new(),
            title: "Add CSV export".to_owned(),
            description: Some("Users export reports as CSV.".to_owned()),
            priority: Priority::High,
            dependencies: Vec::new(),
            status: TaskStatus::InProgress,
            current_step: Some("code-generation".to_owned()),
            step_started_at_unix_secs: BTreeMap::new(),
            agent_activity: Vec::new(),
            approval_history: Vec::new(),
            abandonment_note: None,
            worktree: None,
            attempt: 2,
            recovery_history: Vec::new(),
            automatic_revisions: 1,
            published: None,
            release: None,
            requirement_ids: Vec::new(),
            created_at_unix_secs: 1,
            updated_at_unix_secs: 1,
        }
    }

    fn activity(attempt: u32, status: AgentStatus, detail: &str) -> AgentActivity {
        AgentActivity {
            attempt,
            agent_name: "reviewer".to_owned(),
            step: "code-review".to_owned(),
            status,
            detail: Some(detail.to_owned()),
            updated_at_unix_secs: 1,
        }
    }

    fn catalog() -> AgentProfileCatalog {
        AgentProfileCatalog::from_profiles(vec![AgentProfile {
            id: "security".to_owned(),
            description: "Security review".to_owned(),
            steps: vec!["code-review".to_owned()],
            skills: Vec::new(),
            extensions: Vec::new(),
            mcp: None,
            model: None,
            timeout_minutes: None,
        }])
        .unwrap()
    }

    struct RecordingAgents(std::sync::Mutex<Vec<(String, String)>>);

    impl crate::workflow::AgentAdapter for RecordingAgents {
        fn run(
            &self,
            _task: &Task,
            step: WorkflowStep,
            agent: &crate::workflow::AgentPlan,
        ) -> Result<crate::workflow::AgentReport, String> {
            self.0
                .lock()
                .unwrap()
                .push((step.as_str().to_owned(), agent.name.clone()));
            Ok(crate::workflow::AgentReport::success("done"))
        }

        fn cancel(&self, _: &Task, _: WorkflowStep, _: &crate::workflow::AgentPlan) {}
    }

    /// Answers with a plan for review and verification, and fails for every other step.
    struct ScriptedPlanner;

    impl super::StepPlanner for ScriptedPlanner {
        fn plan(&self, _task: &Task, step: WorkflowStep, prompt: &str) -> Result<String, String> {
            assert!(prompt.contains(step.as_str()));
            match step {
                WorkflowStep::SpecificationGeneration => Ok(
                    "{\"agents\": [{\"name\": \"writer\", \"prompt\": \"Specify it\"}]}".to_owned(),
                ),
                WorkflowStep::CodeReview | WorkflowStep::Verification => Ok(
                    "{\"agents\": [{\"name\": \"quality\", \"prompt\": \"Check it\"}, {\"name\": \"security\", \"prompt\": \"Probe it\"}]}".to_owned(),
                ),
                _ => Err("the planner timed out".to_owned()),
            }
        }
    }

    #[test]
    fn the_step_loop_runs_agent_steps_until_a_human_or_rust_step() {
        let root = tempfile::tempdir().unwrap();
        let manager = crate::project::ProjectManager::new(root.path()).unwrap();
        let project = manager.create_project("Loop".to_owned(), None).unwrap();
        let task = manager
            .create_task(
                project.id,
                "Build".to_owned(),
                None,
                Priority::High,
                Vec::new(),
            )
            .unwrap();
        manager.start_next_task(project.id).unwrap();
        let agents = RecordingAgents(std::sync::Mutex::new(Vec::new()));
        let engine = crate::workflow::WorkflowEngine::new(
            crate::project::ProjectManager::new(root.path()).unwrap(),
            agents,
        );
        let mut progress = Vec::new();

        let waiting = super::run_task(
            &engine,
            &ScriptedPlanner,
            &AgentProfileCatalog::default(),
            project.id,
            task.id,
            &mut |line: &str| progress.push(line.to_owned()),
        )
        .unwrap();
        assert_eq!(
            waiting.current_step.as_deref(),
            Some("specification-approval")
        );

        manager
            .record_approval(
                project.id,
                task.id,
                ApprovalGate::Specifications,
                ApprovalDecision::Approved,
                None,
            )
            .unwrap();
        let waiting = super::run_task(
            &engine,
            &ScriptedPlanner,
            &AgentProfileCatalog::default(),
            project.id,
            task.id,
            &mut |line: &str| progress.push(line.to_owned()),
        )
        .unwrap();

        assert_eq!(waiting.current_step.as_deref(), Some("final-approval"));
        let mut ran = engine_agents(&engine);
        ran.sort();
        assert_eq!(
            ran,
            [
                ("code-generation", "builder"),
                ("code-review", "quality"),
                ("code-review", "security"),
                ("specification-generation", "writer"),
                ("verification", "quality"),
                ("verification", "security"),
            ]
            .map(|(step, name)| (step.to_owned(), name.to_owned()))
        );
        let fallback = waiting
            .agent_activity
            .iter()
            .find(|activity| activity.agent_name == "orchestrator")
            .unwrap();
        assert!(
            fallback
                .detail
                .as_deref()
                .unwrap()
                .contains("the planner timed out")
        );
        assert!(
            progress
                .iter()
                .any(|line| line.contains("code-review: 2 agents"))
        );
    }

    fn engine_agents(
        engine: &crate::workflow::WorkflowEngine<RecordingAgents>,
    ) -> Vec<(String, String)> {
        engine.adapter().0.lock().unwrap().clone()
    }

    #[test]
    fn the_context_keeps_only_the_current_attempts_findings_and_notes() {
        let mut task = task();
        task.agent_activity = vec![
            activity(1, AgentStatus::ChangesRequested, "old finding"),
            activity(2, AgentStatus::ChangesRequested, "missing CSV header"),
            activity(2, AgentStatus::Completed, "looks fine"),
        ];
        task.approval_history = vec![ApprovalRecord {
            attempt: 2,
            gate: ApprovalGate::FinalCode,
            decision: ApprovalDecision::Rejected,
            note: Some("Quote fields with commas".to_owned()),
            decided_at_unix_secs: 1,
        }];
        task.recovery_history = vec![RecoveryRecord {
            attempt: 2,
            action: RecoveryAction::Clarify,
            note: Some("Use semicolons for EU locales".to_owned()),
            discarded_worktree: None,
            resumed_at_unix_secs: 1,
        }];

        let context = StepContext::for_task(&task, WorkflowStep::CodeGeneration);

        assert_eq!(context.findings, vec!["reviewer: missing CSV header"]);
        assert_eq!(
            context.revision_note.as_deref(),
            Some("Quote fields with commas")
        );
        assert_eq!(
            context.clarification.as_deref(),
            Some("Use semicolons for EU locales")
        );
    }

    #[test]
    fn the_prompt_explains_the_step_its_limits_and_the_profiles() {
        let mut task = task();
        task.agent_activity = vec![activity(2, AgentStatus::ChangesRequested, "missing header")];
        let prompt = step_planning_prompt(
            &task,
            WorkflowStep::CodeReview,
            &StepContext::for_task(&task, WorkflowStep::CodeReview),
            &catalog(),
        );

        assert!(prompt.contains("Add CSV export"));
        assert!(prompt.contains("code-review"));
        assert!(prompt.contains("1 to 4 agents"));
        assert!(prompt.contains("security: Security review"));
        assert!(prompt.contains("missing header"));
        assert!(prompt.contains("\"agents\""));
    }

    #[test]
    fn a_step_plan_is_read_and_validated_like_a_run_step_request() {
        let catalog = catalog();
        let plan = parse_step_plan(
            "Plan:\n```json\n{\"agents\": [{\"name\": \"quality\", \"prompt\": \"Check tests\"}, {\"name\": \"security\", \"prompt\": \"Check input handling\", \"profile_id\": \"security\"}]}\n```",
            WorkflowStep::CodeReview,
            &catalog,
        )
        .unwrap();
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[1].profile_id.as_deref(), Some("security"));

        for answer in [
            "no plan",
            "{\"agents\": []}",
            "{\"agents\": [{\"name\": \"Bad Name\", \"prompt\": \"x\"}]}",
            "{\"agents\": [{\"name\": \"a\", \"prompt\": \"x\", \"profile_id\": \"missing\"}]}",
        ] {
            assert!(
                parse_step_plan(answer, WorkflowStep::CodeReview, &catalog).is_err(),
                "{answer}"
            );
        }
        assert!(
            parse_step_plan(
                "{\"agents\": [{\"name\": \"a\", \"prompt\": \"x\"}, {\"name\": \"b\", \"prompt\": \"y\"}]}",
                WorkflowStep::SpecificationGeneration,
                &catalog
            )
            .is_err(),
            "specification generation has one author"
        );
    }

    #[test]
    fn the_default_plan_is_one_agent_carrying_the_context() {
        let mut task = task();
        task.agent_activity = vec![activity(2, AgentStatus::ChangesRequested, "missing header")];

        for step in [
            WorkflowStep::SpecificationGeneration,
            WorkflowStep::CodeGeneration,
            WorkflowStep::CodeReview,
            WorkflowStep::Verification,
            WorkflowStep::Documentation,
        ] {
            let plan = default_plan(&task, step, &StepContext::for_task(&task, step));
            assert_eq!(plan.len(), 1, "{}", step.as_str());
            assert!(plan[0].prompt.contains("Add CSV export"));
            assert!(crate::workflow::validate_agents(step, &plan).is_ok());
        }
        let build = default_plan(
            &task,
            WorkflowStep::CodeGeneration,
            &StepContext::for_task(&task, WorkflowStep::CodeGeneration),
        );
        assert!(build[0].prompt.contains("missing header"));
    }
}
