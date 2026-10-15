//! Turning a project's requirements into a reviewed plan of tasks.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model::{
    Priority, Project, RequirementId, RequirementKind, Task, TaskId, TaskStatus, normalize_text,
};

/// Most tasks one plan may propose; larger plans are a sign the requirements need splitting.
pub const MAX_PLANNED_TASKS: usize = 50;
const MAX_TITLE_CHARS: usize = 200;
const MAX_DESCRIPTION_CHARS: usize = 10_000;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PlanError {
    #[error("the planner's answer contained no JSON plan")]
    NoPlan,
    #[error("the plan is not valid: {0}")]
    Invalid(String),
    #[error("the plan proposes no tasks")]
    Empty,
    #[error("the plan proposes more than {MAX_PLANNED_TASKS} tasks")]
    TooMany,
    #[error("two planned tasks share the key {0:?}")]
    DuplicateKey(String),
    #[error("two planned tasks share the title {0:?}")]
    DuplicateTitle(String),
    #[error("a planned task refers to the unknown requirement {0}")]
    UnknownRequirement(String),
    #[error("a planned task depends on the unknown task {0:?}")]
    UnknownDependency(String),
    #[error("planned task dependencies form a cycle")]
    Cycle,
    #[error("no planned task covers requirement {0}")]
    Uncovered(String),
}

/// A task the planner proposes. Dependencies are indexes into the plan.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PlannedTask {
    pub title: String,
    pub description: Option<String>,
    pub priority: Priority,
    pub depends_on: Vec<usize>,
    pub requirement_ids: Vec<RequirementId>,
    /// An open task with the same title that this planned task reuses instead of duplicating.
    pub existing_task: Option<TaskId>,
}

/// A validated plan waiting for a human to apply or discard it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Plan {
    pub tasks: Vec<PlannedTask>,
    #[serde(default)]
    pub created_at_unix_secs: u64,
}

impl Plan {
    /// Plan indexes ordered so every task comes after the tasks it depends on, keeping the
    /// planner's order otherwise.
    pub fn creation_order(&self) -> Vec<usize> {
        let mut placed = vec![false; self.tasks.len()];
        let mut order = Vec::with_capacity(self.tasks.len());
        while order.len() < self.tasks.len() {
            let Some(next) = (0..self.tasks.len()).find(|index| {
                !placed[*index]
                    && self.tasks[*index]
                        .depends_on
                        .iter()
                        .all(|dependency| placed[*dependency])
            }) else {
                break;
            };
            placed[next] = true;
            order.push(next);
        }
        order
    }
}

#[derive(Deserialize)]
struct RawPlan {
    tasks: Vec<RawTask>,
}

#[derive(Deserialize)]
struct RawTask {
    key: String,
    title: String,
    #[serde(default)]
    description: Option<String>,
    priority: Priority,
    #[serde(default)]
    depends_on: Vec<String>,
    #[serde(default)]
    requirements: Vec<String>,
}

fn kind_label(kind: RequirementKind) -> &'static str {
    match kind {
        RequirementKind::Functional => "functional",
        RequirementKind::NonFunctional => "non-functional",
    }
}

fn is_open(task: &Task) -> bool {
    !matches!(task.status, TaskStatus::Completed | TaskStatus::Rejected)
}

/// The planner's instructions: the project, its numbered requirements, the open tasks, and the
/// exact answer format.
pub fn planning_prompt(project: &Project, tasks: &[Task]) -> String {
    let requirements = project
        .requirements
        .iter()
        .enumerate()
        .map(|(index, requirement)| {
            format!(
                "R{} ({}): {}",
                index + 1,
                kind_label(requirement.kind),
                requirement.text
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let open = tasks
        .iter()
        .filter(|task| is_open(task))
        .map(|task| {
            format!(
                "- {} ({})",
                task.title,
                match task.status {
                    TaskStatus::InProgress => "in progress",
                    TaskStatus::Error => "error",
                    _ => "pending",
                }
            )
        })
        .collect::<Vec<_>>();
    let open = if open.is_empty() {
        "(none)".to_owned()
    } else {
        open.join("\n")
    };
    format!(
        "Plan the work for the project \"{name}\"{description}.\n\n\
         Requirements:\n{requirements}\n\n\
         Open tasks that already exist:\n{open}\n\n\
         Read the repository and its OpenSpec specifications (openspec/specs/) to learn what already exists. \
         Then split the requirements into small tasks that can each be specified, implemented, reviewed, and merged on their own. \
         Every requirement must be covered by at least one task; a non-functional requirement may be covered by the tasks it constrains. \
         If an open task already covers some work, reuse its exact title instead of proposing a new one.\n\n\
         Answer with one fenced json block and nothing after it, in this format:\n\
         ```json\n\
         {{\"tasks\": [{{\"key\": \"short-id\", \"title\": \"Imperative title\", \"description\": \"What to build and how to verify it\", \
         \"priority\": \"high\" | \"medium\" | \"low\", \"depends_on\": [\"key of a task that must finish first\"], \"requirements\": [\"R1\"]}}]}}\n\
         ```\n\
         Use at most {MAX_PLANNED_TASKS} tasks. Dependencies refer to keys in the same plan and must not form a cycle.",
        name = project.name,
        description = project
            .description
            .as_deref()
            .map(|description| format!(" ({description})"))
            .unwrap_or_default(),
    )
}

/// The JSON object in the planner's answer: the last fenced json block, or else the outermost
/// braces.
fn plan_json(answer: &str) -> Option<&str> {
    if let Some(start) = answer.rfind("```json") {
        let body = &answer[start + "```json".len()..];
        return Some(body.split("```").next().unwrap_or(body).trim());
    }
    let start = answer.find('{')?;
    let end = answer.rfind('}')?;
    (start < end).then(|| &answer[start..=end])
}

/// Reads and validates the planner's answer against the project's requirements and its open
/// tasks.
pub fn parse_plan(answer: &str, project: &Project, tasks: &[Task]) -> Result<Plan, PlanError> {
    let json = plan_json(answer).ok_or(PlanError::NoPlan)?;
    let raw: RawPlan =
        serde_json::from_str(json).map_err(|error| PlanError::Invalid(error.to_string()))?;
    if raw.tasks.is_empty() {
        return Err(PlanError::Empty);
    }
    if raw.tasks.len() > MAX_PLANNED_TASKS {
        return Err(PlanError::TooMany);
    }

    let mut keys = HashMap::new();
    for (index, task) in raw.tasks.iter().enumerate() {
        let key = task.key.trim();
        if key.is_empty() {
            return Err(PlanError::Invalid("a planned task has no key".to_owned()));
        }
        if keys.insert(key, index).is_some() {
            return Err(PlanError::DuplicateKey(key.to_owned()));
        }
    }
    let open_titles = tasks
        .iter()
        .filter(|task| is_open(task))
        .map(|task| (normalize_text(&task.title).to_lowercase(), task.id))
        .collect::<HashMap<_, _>>();

    let mut titles = HashSet::new();
    let mut covered = HashSet::new();
    let mut planned = Vec::with_capacity(raw.tasks.len());
    for task in &raw.tasks {
        let title = normalize_text(&task.title);
        if title.is_empty() || title.chars().count() > MAX_TITLE_CHARS {
            return Err(PlanError::Invalid(format!(
                "task {:?} needs a title of 1 to {MAX_TITLE_CHARS} characters",
                task.key
            )));
        }
        let title_key = title.to_lowercase();
        if !titles.insert(title_key.clone()) {
            return Err(PlanError::DuplicateTitle(title_key));
        }
        let description = task
            .description
            .as_deref()
            .map(str::trim)
            .filter(|description| !description.is_empty())
            .map(str::to_owned);
        if description
            .as_deref()
            .is_some_and(|description| description.chars().count() > MAX_DESCRIPTION_CHARS)
        {
            return Err(PlanError::Invalid(format!(
                "task {:?} has a description over {MAX_DESCRIPTION_CHARS} characters",
                task.key
            )));
        }
        let mut requirement_ids = Vec::new();
        for reference in &task.requirements {
            let index = reference
                .trim()
                .strip_prefix(['R', 'r'])
                .and_then(|number| number.parse::<usize>().ok())
                .filter(|number| (1..=project.requirements.len()).contains(number))
                .ok_or_else(|| PlanError::UnknownRequirement(reference.trim().to_owned()))?;
            covered.insert(index);
            let id = project.requirements[index - 1].id;
            if !requirement_ids.contains(&id) {
                requirement_ids.push(id);
            }
        }
        let mut depends_on = Vec::new();
        for dependency in &task.depends_on {
            let index = *keys
                .get(dependency.trim())
                .ok_or_else(|| PlanError::UnknownDependency(dependency.trim().to_owned()))?;
            if !depends_on.contains(&index) {
                depends_on.push(index);
            }
        }
        planned.push(PlannedTask {
            existing_task: open_titles.get(&title_key).copied(),
            title,
            description,
            priority: task.priority.clone(),
            depends_on,
            requirement_ids,
        });
    }
    if let Some(missing) = (1..=project.requirements.len()).find(|index| !covered.contains(index)) {
        return Err(PlanError::Uncovered(format!("R{missing}")));
    }
    let plan = Plan {
        tasks: planned,
        created_at_unix_secs: 0,
    };
    if plan.creation_order().len() != plan.tasks.len() {
        return Err(PlanError::Cycle);
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::{PlanError, parse_plan, planning_prompt};
    use crate::model::{
        Priority, Project, ProjectId, ProjectStatus, PublishMode, Requirement, RequirementId,
        RequirementKind, Task, TaskId, TaskStatus,
    };

    fn project(requirements: &[(&str, RequirementKind)]) -> Project {
        Project {
            id: ProjectId::new(),
            name: "Exporter".to_owned(),
            description: Some("Exports reports".to_owned()),
            status: ProjectStatus::Active,
            task_ids: Vec::new(),
            repository: None,
            check_command: None,
            check_timeout_minutes: None,
            publish: PublishMode::Local,
            remote: None,
            releases: false,
            requirements: requirements
                .iter()
                .map(|(text, kind)| Requirement {
                    id: RequirementId::new(),
                    kind: *kind,
                    text: (*text).to_owned(),
                    created_at_unix_secs: 1,
                })
                .collect(),
            max_parallel_tasks: 1,
            sandbox: false,
            created_at_unix_secs: 1,
            updated_at_unix_secs: 1,
        }
    }

    fn open_task(title: &str) -> Task {
        Task {
            id: TaskId::new(),
            project_id: ProjectId::new(),
            title: title.to_owned(),
            description: None,
            priority: Priority::Medium,
            dependencies: Vec::new(),
            status: TaskStatus::Pending,
            current_step: None,
            step_started_at_unix_secs: Default::default(),
            agent_activity: Vec::new(),
            approval_history: Vec::new(),
            abandonment_note: None,
            worktree: None,
            attempt: 1,
            recovery_history: Vec::new(),
            automatic_revisions: 0,
            published: None,
            release: None,
            requirement_ids: Vec::new(),
            created_at_unix_secs: 1,
            updated_at_unix_secs: 1,
        }
    }

    const PLAN: &str = r#"I read the specs. Here is the plan:

```json
{
  "tasks": [
    {"key": "csv", "title": "  Add CSV   export ", "description": "Write rows as CSV.",
     "priority": "high", "depends_on": [], "requirements": ["R1"]},
    {"key": "fast", "title": "Stream large exports", "priority": "medium",
     "depends_on": ["csv"], "requirements": ["R2", "R1"]}
  ]
}
```
"#;

    #[test]
    fn the_prompt_numbers_requirements_and_lists_open_tasks() {
        let project = project(&[
            ("Users can export CSV", RequirementKind::Functional),
            (
                "Exports finish within 2 seconds",
                RequirementKind::NonFunctional,
            ),
        ]);
        let prompt = planning_prompt(&project, &[open_task("Set up CI")]);

        assert!(prompt.contains("R1 (functional): Users can export CSV"));
        assert!(prompt.contains("R2 (non-functional): Exports finish within 2 seconds"));
        assert!(prompt.contains("- Set up CI (pending)"));
        assert!(prompt.contains("\"depends_on\""));
    }

    #[test]
    fn a_plan_is_read_from_the_answer_normalized_and_resolved() {
        let project = project(&[
            ("Users can export CSV", RequirementKind::Functional),
            (
                "Exports finish within 2 seconds",
                RequirementKind::NonFunctional,
            ),
        ]);

        let plan = parse_plan(PLAN, &project, &[]).unwrap();

        assert_eq!(plan.tasks.len(), 2);
        assert_eq!(plan.tasks[0].title, "Add CSV export");
        assert_eq!(plan.tasks[0].priority, Priority::High);
        assert_eq!(
            plan.tasks[0].requirement_ids,
            vec![project.requirements[0].id]
        );
        assert_eq!(
            plan.tasks[1].requirement_ids,
            vec![project.requirements[1].id, project.requirements[0].id]
        );
        assert_eq!(plan.tasks[1].depends_on, vec![0]);
        assert_eq!(plan.tasks[1].description, None);
        assert_eq!(plan.tasks[0].existing_task, None);
    }

    #[test]
    fn planned_tasks_matching_open_tasks_reuse_them() {
        let project = project(&[
            ("Users can export CSV", RequirementKind::Functional),
            (
                "Exports finish within 2 seconds",
                RequirementKind::NonFunctional,
            ),
        ]);
        let existing = open_task("add csv export");

        let plan = parse_plan(PLAN, &project, std::slice::from_ref(&existing)).unwrap();

        assert_eq!(plan.tasks[0].existing_task, Some(existing.id));
        assert_eq!(plan.tasks[1].existing_task, None);
    }

    #[test]
    fn invalid_plans_are_rejected_with_the_reason() {
        let project = project(&[
            ("Users can export CSV", RequirementKind::Functional),
            (
                "Exports finish within 2 seconds",
                RequirementKind::NonFunctional,
            ),
        ]);
        let check = |json: &str| parse_plan(json, &project, &[]).unwrap_err();

        assert!(matches!(check("no json here"), PlanError::NoPlan));
        assert!(matches!(check(r#"{"tasks": []}"#), PlanError::Empty));
        assert!(matches!(
            check(
                r#"{"tasks": [{"key": "a", "title": "A", "priority": "high", "depends_on": [], "requirements": ["R1"]}]}"#
            ),
            PlanError::Uncovered(requirement) if requirement == "R2"
        ));
        assert!(matches!(
            check(
                r#"{"tasks": [{"key": "a", "title": "A", "priority": "high", "depends_on": [], "requirements": ["R1", "R2", "R9"]}]}"#
            ),
            PlanError::UnknownRequirement(reference) if reference == "R9"
        ));
        assert!(matches!(
            check(
                r#"{"tasks": [{"key": "a", "title": "A", "priority": "high", "depends_on": ["b"], "requirements": ["R1", "R2"]}]}"#
            ),
            PlanError::UnknownDependency(key) if key == "b"
        ));
        assert!(matches!(
            check(
                r#"{"tasks": [
                    {"key": "a", "title": "A", "priority": "high", "depends_on": ["b"], "requirements": ["R1"]},
                    {"key": "b", "title": "B", "priority": "high", "depends_on": ["a"], "requirements": ["R2"]}
                ]}"#
            ),
            PlanError::Cycle
        ));
        assert!(matches!(
            check(
                r#"{"tasks": [
                    {"key": "a", "title": "Same", "priority": "high", "depends_on": [], "requirements": ["R1"]},
                    {"key": "b", "title": " same ", "priority": "low", "depends_on": [], "requirements": ["R2"]}
                ]}"#
            ),
            PlanError::DuplicateTitle(title) if title == "same"
        ));
        assert!(matches!(
            check(
                r#"{"tasks": [{"key": "a", "title": "A", "priority": "urgent", "depends_on": [], "requirements": ["R1", "R2"]}]}"#
            ),
            PlanError::Invalid(_)
        ));
    }

    #[test]
    fn creation_order_puts_dependencies_first() {
        let project = project(&[("Users can export CSV", RequirementKind::Functional)]);
        let plan = parse_plan(
            r#"{"tasks": [
                {"key": "c", "title": "C", "priority": "low", "depends_on": ["b"], "requirements": ["R1"]},
                {"key": "a", "title": "A", "priority": "low", "depends_on": [], "requirements": []},
                {"key": "b", "title": "B", "priority": "low", "depends_on": ["a"], "requirements": []}
            ]}"#,
            &project,
            &[],
        )
        .unwrap();

        assert_eq!(plan.creation_order(), vec![1, 2, 0]);
    }
}
