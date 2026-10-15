use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model::WorkflowStep;

const PROFILE_FILE: &str = "agent-profiles.json";
const MAX_PROFILE_FILE_BYTES: u64 = 1024 * 1024;
const MAX_PROFILES: usize = 100;
const MAX_RESOURCES_PER_PROFILE: usize = 32;
pub const MAX_TIMEOUT_MINUTES: u32 = 480;
const MAX_MODEL_CHARS: usize = 128;

#[derive(Clone, Debug, Default)]
pub struct AgentProfileCatalog {
    profiles: Vec<AgentProfile>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentProfile {
    pub id: String,
    pub description: String,
    pub steps: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<McpProfile>,
    /// Pi `--model` pattern for workers using this profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Overrides the step's default agent time limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_minutes: Option<u32>,
}

impl AgentProfile {
    /// Checks the profile on its own: ID, description, steps, model, time limit, and that every
    /// resource path exists. Duplicate IDs are checked when the catalog is saved.
    pub fn validate(&self) -> Result<(), AgentProfileError> {
        validate_profile(self)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
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
    #[error(
        "profile {profile} model must be 1-{MAX_MODEL_CHARS} characters of letters, digits, and . _ : / -, starting with a letter or digit"
    )]
    InvalidModel { profile: String },
    #[error("profile {profile} timeout_minutes must be between 1 and {MAX_TIMEOUT_MINUTES}")]
    InvalidTimeout { profile: String },
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AgentProfileFile {
    version: u64,
    profiles: Vec<AgentProfile>,
}

/// Path of the catalog, refusing a symlink or a non-file in its place.
fn catalog_path(data_dir: &Path) -> Result<(PathBuf, bool), AgentProfileError> {
    let path = data_dir.join(PROFILE_FILE);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(AgentProfileError::InvalidResourcePath {
                profile: "catalog".to_owned(),
                kind: "profile catalog",
                path,
            })
        }
        Ok(_) => Ok((path, true)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok((path, false)),
        Err(error) => Err(error.into()),
    }
}

/// Parses and version-checks the catalog without validating its profiles.
fn read_catalog(data_dir: &Path) -> Result<Vec<AgentProfile>, AgentProfileError> {
    let (path, exists) = catalog_path(data_dir)?;
    if !exists {
        return Ok(Vec::new());
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
    Ok(file.profiles)
}

/// Profiles as stored, without validation, so an editor can show and fix an invalid catalog.
pub fn load_profiles_for_editing(data_dir: &Path) -> Result<Vec<AgentProfile>, AgentProfileError> {
    read_catalog(data_dir)
}

/// Validates the whole catalog and replaces the file atomically, so a step loading it at the
/// same time reads either the old or the new catalog.
pub fn save_profiles(
    data_dir: &Path,
    profiles: Vec<AgentProfile>,
) -> Result<AgentProfileCatalog, AgentProfileError> {
    let catalog = AgentProfileCatalog::from_profiles(profiles)?;
    write_catalog(data_dir, &catalog.profiles)?;
    Ok(catalog)
}

/// Saves an edit made in the TUI. The `edited` profile must be fully valid and every ID valid and
/// unique; other profiles are kept as they are, so one broken profile never blocks fixing another.
pub fn save_edited_profiles(
    data_dir: &Path,
    profiles: Vec<AgentProfile>,
    edited: Option<&str>,
) -> Result<(), AgentProfileError> {
    if profiles.len() > MAX_PROFILES {
        return Err(AgentProfileError::TooManyProfiles);
    }
    let mut ids = std::collections::HashSet::with_capacity(profiles.len());
    for profile in &profiles {
        if !valid_profile_id(&profile.id) || !ids.insert(&profile.id) {
            return Err(AgentProfileError::InvalidProfileId(profile.id.clone()));
        }
        if edited == Some(profile.id.as_str()) {
            validate_profile(profile)?;
        }
    }
    write_catalog(data_dir, &profiles)
}

fn write_catalog(data_dir: &Path, profiles: &[AgentProfile]) -> Result<(), AgentProfileError> {
    let (path, _) = catalog_path(data_dir)?;
    let file = AgentProfileFile {
        version: 1,
        profiles: profiles.to_vec(),
    };
    let mut bytes = serde_json::to_vec_pretty(&file)?;
    bytes.push(b'\n');
    if bytes.len() as u64 > MAX_PROFILE_FILE_BYTES {
        return Err(AgentProfileError::CatalogTooLarge);
    }
    let mut temporary = tempfile::NamedTempFile::new_in(data_dir)?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(&path)
        .map_err(|error| AgentProfileError::Io(error.error))?;
    Ok(())
}

impl AgentProfileCatalog {
    pub fn load(data_dir: &Path) -> Result<Self, AgentProfileError> {
        Self::from_profiles(read_catalog(data_dir)?)
    }

    pub fn profiles(&self) -> &[AgentProfile] {
        &self.profiles
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

    if profile
        .model
        .as_deref()
        .is_some_and(|model| !valid_model(model))
    {
        return Err(AgentProfileError::InvalidModel {
            profile: profile.id.clone(),
        });
    }
    if profile
        .timeout_minutes
        .is_some_and(|minutes| minutes == 0 || minutes > MAX_TIMEOUT_MINUTES)
    {
        return Err(AgentProfileError::InvalidTimeout {
            profile: profile.id.clone(),
        });
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

pub(crate) fn valid_model(model: &str) -> bool {
    // A leading '-' would be read by Pi as an option rather than the model value.
    model
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_alphanumeric())
        && model.len() <= MAX_MODEL_CHARS
        && model
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | ':' | '/' | '-'))
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
            model: None,
            timeout_minutes: None,
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
            model: Some("anthropic/claude-sonnet-4:high".to_owned()),
            timeout_minutes: Some(45),
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

        for model in ["model; rm -rf /", "--extension", "-x"] {
            let mut bad_model = profile("security-reviewer", &["code-review"]);
            bad_model.model = Some(model.to_owned());
            assert!(matches!(
                AgentProfileCatalog::from_profiles(vec![bad_model]),
                Err(AgentProfileError::InvalidModel { .. })
            ));
        }
        for minutes in [0, super::MAX_TIMEOUT_MINUTES + 1] {
            let mut bad_timeout = profile("security-reviewer", &["code-review"]);
            bad_timeout.timeout_minutes = Some(minutes);
            assert!(matches!(
                AgentProfileCatalog::from_profiles(vec![bad_timeout]),
                Err(AgentProfileError::InvalidTimeout { .. })
            ));
        }
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

    #[test]
    fn saved_catalogs_round_trip_without_empty_fields() {
        let temp = tempfile::tempdir().unwrap();
        let skill = temp.path().join("review-skill");
        fs::create_dir_all(&skill).unwrap();
        let mut reviewer = profile("reviewer", &["code-review", "verification"]);
        reviewer.skills.push(skill.clone());
        reviewer.model = Some("provider/model".to_owned());
        reviewer.timeout_minutes = Some(20);
        let plain = profile("writer", &["code-generation"]);

        let saved =
            super::save_profiles(temp.path(), vec![reviewer.clone(), plain.clone()]).unwrap();
        assert_eq!(saved.profiles(), &[reviewer.clone(), plain.clone()]);
        let json = fs::read_to_string(temp.path().join("agent-profiles.json")).unwrap();
        assert!(json.contains("\"version\": 1"));
        assert!(!json.contains("extensions") && !json.contains("mcp"));
        assert_eq!(
            AgentProfileCatalog::load(temp.path()).unwrap().profiles(),
            &[reviewer, plain]
        );
        // No temporary files are left next to the catalog.
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 2);
    }

    #[test]
    fn invalid_catalogs_open_for_editing_but_are_never_saved() {
        let temp = tempfile::tempdir().unwrap();
        let skill = temp.path().join("skill");
        fs::create_dir_all(&skill).unwrap();
        let mut broken = profile("broken", &["code-review"]);
        broken.skills.push(skill.clone());
        super::save_profiles(temp.path(), vec![broken.clone()]).unwrap();
        let before = fs::read(temp.path().join("agent-profiles.json")).unwrap();
        fs::remove_dir(&skill).unwrap();

        // Workflow loading rejects the catalog, but the editor still sees every profile.
        assert!(AgentProfileCatalog::load(temp.path()).is_err());
        let editable = super::load_profiles_for_editing(temp.path()).unwrap();
        assert_eq!(editable, vec![broken.clone()]);
        assert!(matches!(
            editable[0].validate(),
            Err(AgentProfileError::InvalidResourcePath { .. })
        ));

        for profiles in [
            editable.clone(),
            vec![
                profile("same", &["code-review"]),
                profile("same", &["verification"]),
            ],
            vec![profile("no-steps", &[])],
        ] {
            assert!(super::save_profiles(temp.path(), profiles).is_err());
            assert_eq!(
                fs::read(temp.path().join("agent-profiles.json")).unwrap(),
                before
            );
        }

        // An edit to another profile is saved and keeps the broken one for later fixing.
        let mut profiles = editable.clone();
        profiles.push(profile("other", &["documentation"]));
        super::save_edited_profiles(temp.path(), profiles.clone(), Some("other")).unwrap();
        assert_eq!(
            super::load_profiles_for_editing(temp.path()).unwrap(),
            profiles
        );
        // The edited profile itself must be valid, and IDs must stay unique.
        assert!(
            super::save_edited_profiles(temp.path(), profiles.clone(), Some("broken")).is_err()
        );
        let mut duplicated = profiles.clone();
        duplicated.push(profile("other", &["code-review"]));
        assert!(super::save_edited_profiles(temp.path(), duplicated, None).is_err());

        // Removing the bad path makes it savable again.
        let mut fixed = editable[0].clone();
        fixed.skills.clear();
        super::save_profiles(temp.path(), vec![fixed]).unwrap();
        assert!(AgentProfileCatalog::load(temp.path()).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_catalog_is_never_written_through() {
        let temp = tempfile::tempdir().unwrap();
        let elsewhere = temp.path().join("elsewhere.json");
        fs::write(&elsewhere, "keep").unwrap();
        std::os::unix::fs::symlink(&elsewhere, temp.path().join("agent-profiles.json")).unwrap();
        assert!(super::save_profiles(temp.path(), vec![profile("a", &["code-review"])]).is_err());
        assert!(super::load_profiles_for_editing(temp.path()).is_err());
        assert_eq!(fs::read_to_string(&elsewhere).unwrap(), "keep");
    }
}
