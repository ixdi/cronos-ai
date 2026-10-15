//! Built-in Pi skills that every worker of a workflow step loads, before any profile skills.

use std::{
    io,
    path::{Path, PathBuf},
};

use crate::{
    integrations::{ensure_real_directory, install_embedded_file},
    model::WorkflowStep,
};

pub struct StepSkill {
    pub step: WorkflowStep,
    /// Skill name; also its directory under `<data>/skills/`.
    pub name: &'static str,
    pub content: &'static str,
}

pub const STEP_SKILLS: &[StepSkill] = &[
    StepSkill {
        step: WorkflowStep::SpecificationGeneration,
        name: "cronos-specification-generation",
        content: include_str!("../skills/cronos-specification-generation/SKILL.md"),
    },
    StepSkill {
        step: WorkflowStep::CodeGeneration,
        name: "cronos-code-generation",
        content: include_str!("../skills/cronos-code-generation/SKILL.md"),
    },
    StepSkill {
        step: WorkflowStep::CodeReview,
        name: "cronos-code-review",
        content: include_str!("../skills/cronos-code-review/SKILL.md"),
    },
    StepSkill {
        step: WorkflowStep::Verification,
        name: "cronos-verification",
        content: include_str!("../skills/cronos-verification/SKILL.md"),
    },
    StepSkill {
        step: WorkflowStep::Documentation,
        name: "cronos-documentation",
        content: include_str!("../skills/cronos-documentation/SKILL.md"),
    },
];

/// Installed locations of the built-in step skills.
#[derive(Clone, Debug, Default)]
pub struct StepSkills {
    paths: Vec<(WorkflowStep, PathBuf)>,
}

impl StepSkills {
    pub fn path(&self, step: WorkflowStep) -> Option<&Path> {
        self.paths
            .iter()
            .find(|(skill_step, _)| *skill_step == step)
            .map(|(_, path)| path.as_path())
    }
}

/// Writes the embedded step skills to `<data>/skills/<name>/SKILL.md`, so workers never depend on
/// the source checkout and an edited copy is restored. Unchanged files are left alone.
pub fn install_step_skills(data_dir: &Path) -> io::Result<StepSkills> {
    let root = ensure_real_directory(&data_dir.join("skills"))?;
    let paths = STEP_SKILLS
        .iter()
        .map(|skill| {
            let directory = ensure_real_directory(&root.join(skill.name))?;
            let path = install_embedded_file(&directory, "SKILL.md", skill.content)?;
            Ok((skill.step, path))
        })
        .collect::<io::Result<_>>()?;
    Ok(StepSkills { paths })
}

const PLANNING_SKILL_NAME: &str = "cronos-project-planning";
const PLANNING_SKILL: &str = include_str!("../skills/cronos-project-planning/SKILL.md");

/// Writes the embedded project-planning skill to `<data>/skills/cronos-project-planning/SKILL.md`.
pub fn install_planning_skill(data_dir: &Path) -> io::Result<PathBuf> {
    let root = ensure_real_directory(&data_dir.join("skills"))?;
    let directory = ensure_real_directory(&root.join(PLANNING_SKILL_NAME))?;
    install_embedded_file(&directory, "SKILL.md", PLANNING_SKILL)
}

#[cfg(test)]
mod tests {
    use super::{STEP_SKILLS, install_step_skills};
    use crate::model::WorkflowStep;

    const AGENT_STEPS: [WorkflowStep; 5] = [
        WorkflowStep::SpecificationGeneration,
        WorkflowStep::CodeGeneration,
        WorkflowStep::CodeReview,
        WorkflowStep::Verification,
        WorkflowStep::Documentation,
    ];

    #[test]
    fn every_agent_step_has_a_skill_whose_name_matches_its_directory() {
        for step in AGENT_STEPS {
            let skill = STEP_SKILLS
                .iter()
                .find(|skill| skill.step == step)
                .unwrap_or_else(|| panic!("no skill for {}", step.as_str()));
            assert_eq!(skill.name, format!("cronos-{}", step.as_str()));
            let frontmatter = skill
                .content
                .strip_prefix("---\n")
                .and_then(|rest| rest.split_once("\n---\n"))
                .map(|(frontmatter, _)| frontmatter)
                .expect("SKILL.md starts with YAML frontmatter");
            assert!(frontmatter.contains(&format!("name: {}\n", skill.name)));
            assert!(frontmatter.contains("description: "));
        }
        assert_eq!(STEP_SKILLS.len(), AGENT_STEPS.len());
    }

    #[test]
    fn installs_each_skill_and_resolves_it_by_step() {
        let temp = tempfile::tempdir().unwrap();
        let skills = install_step_skills(temp.path()).unwrap();

        for skill in STEP_SKILLS {
            let path = skills.path(skill.step).unwrap();
            assert_eq!(
                path,
                temp.path().join("skills").join(skill.name).join("SKILL.md")
            );
            assert_eq!(std::fs::read_to_string(path).unwrap(), skill.content);
        }
        assert_eq!(skills.path(WorkflowStep::LocalMerge), None);
    }

    #[test]
    fn reinstalling_restores_edited_skills() {
        let temp = tempfile::tempdir().unwrap();
        let skills = install_step_skills(temp.path()).unwrap();
        let path = skills.path(WorkflowStep::CodeReview).unwrap().to_owned();
        std::fs::write(&path, "tampered").unwrap();

        install_step_skills(temp.path()).unwrap();

        assert_ne!(std::fs::read_to_string(path).unwrap(), "tampered");
    }

    #[test]
    fn the_planning_skill_is_installed_next_to_the_step_skills() {
        let temp = tempfile::tempdir().unwrap();
        let path = super::install_planning_skill(temp.path()).unwrap();

        assert_eq!(
            path,
            temp.path().join("skills/cronos-project-planning/SKILL.md")
        );
        assert!(
            std::fs::read_to_string(path)
                .unwrap()
                .starts_with("---\nname: cronos-project-planning\n")
        );
    }

    #[cfg(unix)]
    #[test]
    fn refuses_to_write_through_a_symlinked_skill_directory() {
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("skills")).unwrap();
        std::os::unix::fs::symlink(
            outside.path(),
            temp.path().join("skills").join("cronos-code-review"),
        )
        .unwrap();

        assert!(install_step_skills(temp.path()).is_err());
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }
}
