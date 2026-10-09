use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use thiserror::Error;

use crate::model::WorkflowStep;

const PROFILE_FILE: &str = "agent-profiles.json";
const MAX_PROFILE_FILE_BYTES: u64 = 1024 * 1024;
const MAX_PROFILES: usize = 100;
const MAX_RESOURCES_PER_PROFILE: usize = 32;

#[derive(Clone, Debug, Default)]
pub struct AgentProfileCatalog {
    profiles: Vec<AgentProfile>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AgentProfile {
    pub id: String,
    pub description: String,
    pub steps: Vec<String>,
    #[serde(default)]
    pub skills: Vec<PathBuf>,
    #[serde(default)]
    pub extensions: Vec<PathBuf>,
    #[serde(default)]
    pub mcp: Option<McpProfile>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct McpProfile {
    pub adapter: PathBuf,
    pub config: PathBuf,
}

#[derive(Debug, Error)]
pub enum AgentProfileError {
    #[error("could not read agent profile catalog: {0}")]
    Io(#[from] std::io::Error),
    #[error("agent profile catalog exceeds the 1 MiB limit")]
    CatalogTooLarge,
    #[error("invalid agent profile JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported agent profile catalog version {0}")]
    UnsupportedVersion(u64),
    #[error("agent profile catalog has more than {MAX_PROFILES} profiles")]
    TooManyProfiles,
    #[error("invalid or duplicate agent profile ID: {0}")]
    InvalidProfileId(String),
    #[error("profile {profile} must have a nonblank description of at most 500 characters")]
    InvalidDescription { profile: String },
    #[error("profile {profile} must allow at least one agent workflow step")]
    EmptySteps { profile: String },
    #[error("profile {profile} has invalid or non-agent workflow step: {step}")]
    InvalidStep { profile: String, step: String },
    #[error("profile {profile} repeats workflow step: {step}")]
    DuplicateStep { profile: String, step: String },
    #[error("profile {profile} has more than {MAX_RESOURCES_PER_PROFILE} {kind}")]
    TooManyResources { profile: String, kind: &'static str },
    #[error("profile {profile} {kind} path must be absolute: {path}")]
    RelativeResourcePath {
        profile: String,
        kind: &'static str,
        path: PathBuf,
    },
    #[error("profile {profile} {kind} path is missing or has the wrong type: {path}")]
    InvalidResourcePath {
        profile: String,
        kind: &'static str,
        path: PathBuf,
    },
    #[error(
        "profile {profile} MCP config must be a regular JSON file with a nonempty mcpServers object: {path}"
    )]
    InvalidMcpConfig { profile: String, path: PathBuf },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentProfileFile {
    version: u64,
    profiles: Vec<AgentProfile>,
}

impl AgentProfileCatalog {
    pub fn load(data_dir: &Path) -> Result<Self, AgentProfileError> {
        let path = data_dir.join(PROFILE_FILE);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(AgentProfileError::InvalidResourcePath {
                    profile: "catalog".to_owned(),
                    kind: "profile catalog",
                    path,
                });
            }
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(error.into()),
        };
        if metadata.len() > MAX_PROFILE_FILE_BYTES {
            return Err(AgentProfileError::CatalogTooLarge);
        }

        let mut bytes = Vec::new();
        File::open(&path)?
            .take(MAX_PROFILE_FILE_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_PROFILE_FILE_BYTES {
            return Err(AgentProfileError::CatalogTooLarge);
        }
        let file: AgentProfileFile = serde_json::from_slice(&bytes)?;
        if file.version != 1 {
            return Err(AgentProfileError::UnsupportedVersion(file.version));
        }
        Self::from_profiles(file.profiles)
    }

    pub fn from_profiles(profiles: Vec<AgentProfile>) -> Result<Self, AgentProfileError> {
        if profiles.len() > MAX_PROFILES {
            return Err(AgentProfileError::TooManyProfiles);
        }
        let mut ids = std::collections::HashSet::with_capacity(profiles.len());
        for profile in &profiles {
            validate_profile(profile)?;
            if !ids.insert(&profile.id) {
                return Err(AgentProfileError::InvalidProfileId(profile.id.clone()));
            }
        }
        Ok(Self { profiles })
    }

    pub fn for_step(&self, step: WorkflowStep) -> impl Iterator<Item = &AgentProfile> {
        self.profiles.iter().filter(move |profile| {
            profile
                .steps
                .iter()
                .any(|candidate| candidate == step.as_str())
        })
    }

    pub fn get_for_step(&self, id: &str, step: WorkflowStep) -> Option<&AgentProfile> {
        self.for_step(step).find(|profile| profile.id == id)
    }

    pub fn is_empty(&self) -> bool {
        self.profiles.is_empty()
    }
}

fn validate_profile(profile: &AgentProfile) -> Result<(), AgentProfileError> {
    if !valid_profile_id(&profile.id) {
        return Err(AgentProfileError::InvalidProfileId(profile.id.clone()));
    }
    if profile.description.trim().is_empty() || profile.description.chars().count() > 500 {
        return Err(AgentProfileError::InvalidDescription {
            profile: profile.id.clone(),
        });
    }
    if profile.steps.is_empty() {
        return Err(AgentProfileError::EmptySteps {
            profile: profile.id.clone(),
        });
    }
    let mut steps = std::collections::HashSet::with_capacity(profile.steps.len());
    for step in &profile.steps {
        if !WorkflowStep::parse(step).is_some_and(WorkflowStep::runs_agents) {
            return Err(AgentProfileError::InvalidStep {
                profile: profile.id.clone(),
                step: step.clone(),
            });
        }
        if !steps.insert(step) {
            return Err(AgentProfileError::DuplicateStep {
                profile: profile.id.clone(),
                step: step.clone(),
            });
        }
    }

    validate_resource_paths(&profile.id, "skills", &profile.skills, false)?;
    validate_resource_paths(&profile.id, "extensions", &profile.extensions, false)?;
    if let Some(mcp) = &profile.mcp {
        validate_resource_path(&profile.id, "MCP adapter", &mcp.adapter, false)?;
        validate_resource_path(&profile.id, "MCP config", &mcp.config, true)?;
        validate_mcp_config(&profile.id, &mcp.config)?;
    }
    Ok(())
}

fn validate_resource_paths(
    profile: &str,
    kind: &'static str,
    paths: &[PathBuf],
    regular_file: bool,
) -> Result<(), AgentProfileError> {
    if paths.len() > MAX_RESOURCES_PER_PROFILE {
        return Err(AgentProfileError::TooManyResources {
            profile: profile.to_owned(),
            kind,
        });
    }
    for path in paths {
        validate_resource_path(profile, kind, path, regular_file)?;
    }
    Ok(())
}

fn validate_resource_path(
    profile: &str,
    kind: &'static str,
    path: &Path,
    regular_file: bool,
) -> Result<(), AgentProfileError> {
    if !path.is_absolute() {
        return Err(AgentProfileError::RelativeResourcePath {
            profile: profile.to_owned(),
            kind,
            path: path.to_path_buf(),
        });
    }
    let valid = fs::metadata(path).is_ok_and(|metadata| {
        if regular_file {
            metadata.is_file()
        } else {
            metadata.is_file() || metadata.is_dir()
        }
    });
    if !valid {
        return Err(AgentProfileError::InvalidResourcePath {
            profile: profile.to_owned(),
            kind,
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

fn validate_mcp_config(profile: &str, path: &Path) -> Result<(), AgentProfileError> {
    let valid = File::open(path)
        .ok()
        .and_then(|file| {
            let mut bytes = Vec::new();
            file.take(MAX_PROFILE_FILE_BYTES + 1)
                .read_to_end(&mut bytes)
                .ok()?;
            (bytes.len() as u64 <= MAX_PROFILE_FILE_BYTES).then_some(bytes)
        })
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|config| config.get("mcpServers")?.as_object().cloned())
        .is_some_and(|servers| !servers.is_empty());
    if !valid {
        return Err(AgentProfileError::InvalidMcpConfig {
            profile: profile.to_owned(),
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

fn valid_profile_id(id: &str) -> bool {
    let mut chars = id.chars();
    matches!(chars.next(), Some('a'..='z'))
        && id.len() <= 64
        && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '-' | '_'))
}

#[cfg(test)]
mod tests {
    use super::{
        AgentProfile, AgentProfileCatalog, AgentProfileError, AgentProfileFile, McpProfile,
    };
    use std::{fs, path::PathBuf};

    fn profile(id: &str, steps: &[&str]) -> AgentProfile {
        AgentProfile {
            id: id.to_owned(),
            description: format!("{id} agent profile"),
            steps: steps.iter().map(|step| (*step).to_owned()).collect(),
            skills: Vec::new(),
            extensions: Vec::new(),
            mcp: None,
        }
    }

    #[test]
    fn missing_catalog_is_an_empty_profile_set() {
        let root = tempfile::tempdir().unwrap();
        assert!(AgentProfileCatalog::load(root.path()).unwrap().is_empty());
    }

    #[test]
    fn checked_in_profile_examples_validate() {
        let catalog_file: AgentProfileFile =
            serde_json::from_str(include_str!("../examples/agent-profiles.json")).unwrap();
        assert_eq!(catalog_file.version, 1);
        let catalog = AgentProfileCatalog::from_profiles(catalog_file.profiles).unwrap();
        assert_eq!(
            catalog
                .for_step(crate::model::WorkflowStep::CodeReview)
                .count(),
            1
        );

        let root = tempfile::tempdir().unwrap();
        let adapter = root.path().join("mcp-adapter.ts");
        fs::write(&adapter, "export default () => {};\n").unwrap();
        let catalog = AgentProfileCatalog::from_profiles(vec![AgentProfile {
            id: "docs-reviewer".to_owned(),
            description: "Reviews changes with project docs".to_owned(),
            steps: vec!["code-review".to_owned()],
            skills: Vec::new(),
            extensions: Vec::new(),
            mcp: Some(McpProfile {
                adapter,
                config: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("examples/security-review-mcp.json"),
            }),
        }]);
        assert!(catalog.is_ok());
    }

    #[test]
    fn valid_profiles_can_be_selected_by_step_and_id() {
        let catalog = AgentProfileCatalog::from_profiles(vec![profile(
            "security-reviewer",
            &["code-review", "verification"],
        )])
        .unwrap();
        let step_profiles = catalog
            .for_step(crate::model::WorkflowStep::CodeReview)
            .collect::<Vec<_>>();
        assert_eq!(step_profiles.len(), 1);
        assert_eq!(step_profiles[0].id, "security-reviewer");
        assert!(
            catalog
                .get_for_step(
                    "security-reviewer",
                    crate::model::WorkflowStep::Verification
                )
                .is_some()
        );
        assert!(
            catalog
                .get_for_step(
                    "security-reviewer",
                    crate::model::WorkflowStep::Documentation
                )
                .is_none()
        );
    }

    #[test]
    fn malformed_and_oversized_catalogs_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("agent-profiles.json");
        fs::write(&path, "not json").unwrap();
        assert!(matches!(
            AgentProfileCatalog::load(root.path()),
            Err(AgentProfileError::Json(_))
        ));
        fs::write(&path, vec![b' '; 1024 * 1024 + 1]).unwrap();
        assert!(matches!(
            AgentProfileCatalog::load(root.path()),
            Err(AgentProfileError::CatalogTooLarge)
        ));
    }

    #[test]
    fn duplicate_ids_invalid_steps_and_missing_resources_are_rejected() {
        assert!(matches!(
            AgentProfileCatalog::from_profiles(vec![
                profile("security-reviewer", &["code-review"]),
                profile("security-reviewer", &["verification"]),
            ]),
            Err(AgentProfileError::InvalidProfileId(_))
        ));
        assert!(matches!(
            AgentProfileCatalog::from_profiles(vec![profile(
                "security-reviewer",
                &["local-merge"]
            )]),
            Err(AgentProfileError::InvalidStep { .. })
        ));

        let mut missing = profile("security-reviewer", &["code-review"]);
        missing
            .skills
            .push(PathBuf::from("/definitely/missing/skill"));
        assert!(matches!(
            AgentProfileCatalog::from_profiles(vec![missing]),
            Err(AgentProfileError::InvalidResourcePath { .. })
        ));
    }

    #[test]
    fn mcp_requires_an_adapter_and_valid_server_config() {
        let root = tempfile::tempdir().unwrap();
        let adapter = root.path().join("mcp-adapter.ts");
        let config = root.path().join("mcp.json");
        fs::write(&adapter, "export default () => {};\n").unwrap();
        fs::write(
            &config,
            r#"{"mcpServers":{"docs":{"url":"https://example.invalid/mcp"}}}"#,
        )
        .unwrap();

        let mut configured = profile("docs-reviewer", &["code-review"]);
        configured.mcp = Some(McpProfile { adapter, config });
        assert!(AgentProfileCatalog::from_profiles(vec![configured]).is_ok());

        fs::write(root.path().join("mcp.json"), "{}").unwrap();
        let mut invalid = profile("docs-reviewer", &["code-review"]);
        invalid.mcp = Some(McpProfile {
            adapter: root.path().join("mcp-adapter.ts"),
            config: root.path().join("mcp.json"),
        });
        assert!(matches!(
            AgentProfileCatalog::from_profiles(vec![invalid]),
            Err(AgentProfileError::InvalidMcpConfig { .. })
        ));

        fs::write(root.path().join("mcp.json"), vec![b' '; 1024 * 1024 + 1]).unwrap();
        let mut oversized = profile("docs-reviewer", &["code-review"]);
        oversized.mcp = Some(McpProfile {
            adapter: root.path().join("mcp-adapter.ts"),
            config: root.path().join("mcp.json"),
        });
        assert!(matches!(
            AgentProfileCatalog::from_profiles(vec![oversized]),
            Err(AgentProfileError::InvalidMcpConfig { .. })
        ));
    }
}
