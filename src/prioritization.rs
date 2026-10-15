//! Agent-based task prioritization: a fast model ranks the tasks the built-in rules allow to
//! start next. The agent only reorders; dependencies, capacity, and conflicts stay with Rust.

use std::collections::HashSet;

use serde::Deserialize;
use thiserror::Error;

use crate::model::{Priority, Task, TaskId};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RankingError {
    #[error("the prioritizer's answer contained no JSON ranking")]
    NoRanking,
    #[error("the ranking is not valid: {0}")]
    Invalid(String),
    #[error("the ranking is empty")]
    Empty,
    #[error("the ranking names the unknown task {0}")]
    UnknownTask(String),
    #[error("the ranking lists {0} twice")]
    DuplicateTask(String),
}

/// The agent's order of the candidate tasks, best first, and why.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Ranking {
    pub order: Vec<TaskId>,
    pub reason: String,
}

#[derive(Deserialize)]
struct RawRanking {
    order: Vec<String>,
    #[serde(default)]
    reason: String,
}

fn priority_label(priority: &Priority) -> &'static str {
    match priority {
        Priority::High => "high",
        Priority::Medium => "medium",
        Priority::Low => "low",
    }
}

/// Instructions for the prioritizer: the candidates under short keys, the work already running,
/// and the exact answer format.
pub fn ranking_prompt(project: &str, candidates: &[Task], running: &[Task]) -> String {
    let candidates = candidates
        .iter()
        .enumerate()
        .map(|(index, task)| {
            format!(
                "T{} [{}] {}{}",
                index + 1,
                priority_label(&task.priority),
                task.title,
                task.description
                    .as_deref()
                    .map(|description| format!(
                        ": {}",
                        description.chars().take(400).collect::<String>()
                    ))
                    .unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let running = if running.is_empty() {
        "(none)".to_owned()
    } else {
        running
            .iter()
            .map(|task| format!("- {}", task.title))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "Decide which task the project \"{project}\" should start next.\n\n\
         Tasks that may start now:\n{candidates}\n\n\
         Tasks already running:\n{running}\n\n\
         Prefer work that unblocks other work, reduces risk early, and delivers user value; \
         use the stated priority as a strong hint, not a rule. Answer only with JSON:\n\
         {{\"order\": [\"T2\", \"T1\"], \"reason\": \"one sentence on why the first task goes first\"}}"
    )
}

/// The JSON object in the answer: the outermost braces.
fn ranking_json(answer: &str) -> Option<&str> {
    let start = answer.find('{')?;
    let end = answer.rfind('}')?;
    (start < end).then(|| &answer[start..=end])
}

/// Reads the agent's ranking of `candidates`. Candidates it leaves out keep their built-in order
/// after the ranked ones.
pub fn parse_ranking(answer: &str, candidates: &[Task]) -> Result<Ranking, RankingError> {
    let json = ranking_json(answer).ok_or(RankingError::NoRanking)?;
    let raw: RawRanking =
        serde_json::from_str(json).map_err(|error| RankingError::Invalid(error.to_string()))?;
    if raw.order.is_empty() {
        return Err(RankingError::Empty);
    }
    let mut seen = HashSet::new();
    let mut order = Vec::with_capacity(candidates.len());
    for key in &raw.order {
        let normalized = key.trim().to_uppercase();
        let index = normalized
            .strip_prefix('T')
            .and_then(|number| number.parse::<usize>().ok())
            .filter(|number| (1..=candidates.len()).contains(number))
            .ok_or_else(|| RankingError::UnknownTask(key.trim().to_owned()))?;
        if !seen.insert(index) {
            return Err(RankingError::DuplicateTask(normalized));
        }
        order.push(candidates[index - 1].id);
    }
    order.extend(
        candidates
            .iter()
            .enumerate()
            .filter(|(index, _)| !seen.contains(&(index + 1)))
            .map(|(_, task)| task.id),
    );
    Ok(Ranking {
        order,
        reason: raw.reason.trim().chars().take(500).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::{RankingError, parse_ranking, ranking_prompt};
    use crate::model::{Priority, ProjectId, Task, TaskId, TaskStatus};

    fn task(title: &str, priority: Priority, status: TaskStatus) -> Task {
        Task {
            id: TaskId::new(),
            project_id: ProjectId::new(),
            title: title.to_owned(),
            description: Some(format!("About {title}")),
            priority,
            dependencies: Vec::new(),
            status,
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

    #[test]
    fn the_prompt_lists_candidates_by_key_and_the_running_work() {
        let candidates = [
            task("Add export", Priority::High, TaskStatus::Pending),
            task("Fix login", Priority::Low, TaskStatus::Pending),
        ];
        let running = [task("Set up CI", Priority::High, TaskStatus::InProgress)];

        let prompt = ranking_prompt("Exporter", &candidates, &running);

        assert!(prompt.contains("T1 [high] Add export: About Add export"));
        assert!(prompt.contains("T2 [low] Fix login"));
        assert!(prompt.contains("- Set up CI"));
        assert!(prompt.contains("\"order\""));
    }

    #[test]
    fn a_ranking_reorders_the_candidates_and_keeps_omitted_ones_last() {
        let candidates = [
            task("A", Priority::High, TaskStatus::Pending),
            task("B", Priority::Medium, TaskStatus::Pending),
            task("C", Priority::Low, TaskStatus::Pending),
        ];

        let ranking = parse_ranking(
            "Thinking...\n{\"order\": [\"T3\", \"t1\"], \"reason\": \"C unblocks the release\"}",
            &candidates,
        )
        .unwrap();

        assert_eq!(
            ranking.order,
            vec![candidates[2].id, candidates[0].id, candidates[1].id]
        );
        assert_eq!(ranking.reason, "C unblocks the release");
    }

    #[test]
    fn invalid_rankings_are_rejected() {
        let candidates = [task("A", Priority::High, TaskStatus::Pending)];
        let check = |answer: &str| parse_ranking(answer, &candidates).unwrap_err();

        assert_eq!(check("no idea"), RankingError::NoRanking);
        assert!(matches!(
            check("{\"order\": \"T1\"}"),
            RankingError::Invalid(_)
        ));
        assert_eq!(
            check("{\"order\": [\"T7\"], \"reason\": \"x\"}"),
            RankingError::UnknownTask("T7".to_owned())
        );
        assert_eq!(
            check("{\"order\": [\"T1\", \"T1\"], \"reason\": \"x\"}"),
            RankingError::DuplicateTask("T1".to_owned())
        );
        assert_eq!(
            check("{\"order\": [], \"reason\": \"x\"}"),
            RankingError::Empty
        );
    }
}
