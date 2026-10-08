use std::{
    env,
    ffi::OsStr,
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequiredTool {
    Pi,
    Herdr,
    OpenSpec,
    Git,
}

impl RequiredTool {
    pub const ALL: [Self; 4] = [Self::Pi, Self::Herdr, Self::OpenSpec, Self::Git];

    pub fn executable(self) -> &'static str {
        match self {
            Self::Pi => "pi",
            Self::Herdr => "herdr",
            Self::OpenSpec => "openspec",
            Self::Git => "git",
        }
    }
}

#[derive(Debug, Error)]
pub enum IntegrationError {
    #[error("could not run {tool}: {source}")]
    Io {
        tool: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("{tool} exited with {status}: {stderr}")]
    Failed {
        tool: &'static str,
        status: String,
        stderr: String,
    },
    #[error("agent name must match [a-z][a-z0-9_-]{{0,31}}")]
    InvalidAgentName,
    #[error("{0} path must be absolute")]
    RelativePath(&'static str),
}

#[derive(Clone, Debug)]
pub struct ToolCommands {
    pi: PathBuf,
    herdr: PathBuf,
    openspec: PathBuf,
    git: PathBuf,
}

impl Default for ToolCommands {
    fn default() -> Self {
        Self {
            pi: "pi".into(),
            herdr: "herdr".into(),
            openspec: "openspec".into(),
            git: "git".into(),
        }
    }
}

impl ToolCommands {
    pub fn with_paths(pi: PathBuf, herdr: PathBuf, openspec: PathBuf, git: PathBuf) -> Self {
        Self {
            pi,
            herdr,
            openspec,
            git,
        }
    }

    pub fn is_available(&self, tool: RequiredTool) -> bool {
        self.run(tool, &[OsStr::new("--version")]).is_ok()
    }

    pub fn missing_required_tools(&self) -> Vec<RequiredTool> {
        missing_required_tools_with(|tool| self.is_available(tool))
    }

    pub fn pi_rpc_command(
        &self,
        extension: &Path,
        worktree: &Path,
        data_dir: &Path,
        executable_path: &Path,
    ) -> Result<Command, IntegrationError> {
        for (name, path) in [
            ("extension", extension),
            ("worktree", worktree),
            ("data directory", data_dir),
            ("Cronos executable", executable_path),
        ] {
            if !path.is_absolute() {
                return Err(IntegrationError::RelativePath(name));
            }
        }
        let mut command = Command::new(&self.pi);
        command
            .args([
                OsStr::new("--mode"),
                OsStr::new("rpc"),
                OsStr::new("--no-session"),
            ])
            .arg("--extension")
            .arg(extension)
            .current_dir(worktree)
            .env("CRONOS_AI_BIN", executable_path)
            .env("CRONOS_AI_DATA_DIR", data_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        Ok(command)
    }

    pub fn herdr_start_pi_agent(
        &self,
        name: &str,
        pane_id: &str,
    ) -> Result<Output, IntegrationError> {
        if !valid_agent_name(name) {
            return Err(IntegrationError::InvalidAgentName);
        }
        self.run(
            RequiredTool::Herdr,
            &[
                OsStr::new("agent"),
                OsStr::new("start"),
                OsStr::new(name),
                OsStr::new("--kind"),
                OsStr::new("pi"),
                OsStr::new("--pane"),
                OsStr::new(pane_id),
            ],
        )
    }

    pub fn herdr_prompt_agent(&self, name: &str, prompt: &str) -> Result<Output, IntegrationError> {
        if !valid_agent_name(name) {
            return Err(IntegrationError::InvalidAgentName);
        }
        self.run(
            RequiredTool::Herdr,
            &[
                OsStr::new("agent"),
                OsStr::new("prompt"),
                OsStr::new(name),
                OsStr::new(prompt),
                OsStr::new("--wait"),
                OsStr::new("--timeout"),
                OsStr::new("120000"),
            ],
        )
    }

    pub fn herdr_cancel_agent(&self, name: &str) -> Result<Output, IntegrationError> {
        if !valid_agent_name(name) {
            return Err(IntegrationError::InvalidAgentName);
        }
        self.run(
            RequiredTool::Herdr,
            &[
                OsStr::new("agent"),
                OsStr::new("send-keys"),
                OsStr::new(name),
                OsStr::new("ctrl+c"),
            ],
        )?;
        self.run(
            RequiredTool::Herdr,
            &[
                OsStr::new("agent"),
                OsStr::new("wait"),
                OsStr::new(name),
                OsStr::new("--timeout"),
                OsStr::new("15000"),
            ],
        )
    }

    pub fn herdr_read_agent(&self, name: &str, lines: usize) -> Result<Output, IntegrationError> {
        if !valid_agent_name(name) {
            return Err(IntegrationError::InvalidAgentName);
        }
        let lines = lines.to_string();
        self.run(
            RequiredTool::Herdr,
            &[
                OsStr::new("agent"),
                OsStr::new("read"),
                OsStr::new(name),
                OsStr::new("--source"),
                OsStr::new("recent-unwrapped"),
                OsStr::new("--lines"),
                OsStr::new(&lines),
            ],
        )
    }

    pub fn run_openspec(&self, args: &[&OsStr]) -> Result<Output, IntegrationError> {
        self.run(RequiredTool::OpenSpec, args)
    }

    pub fn run_git(&self, args: &[&OsStr]) -> Result<Output, IntegrationError> {
        self.run(RequiredTool::Git, args)
    }

    fn run(&self, tool: RequiredTool, args: &[&OsStr]) -> Result<Output, IntegrationError> {
        let executable = match tool {
            RequiredTool::Pi => &self.pi,
            RequiredTool::Herdr => &self.herdr,
            RequiredTool::OpenSpec => &self.openspec,
            RequiredTool::Git => &self.git,
        };
        let output = Command::new(executable)
            .args(args)
            .output()
            .map_err(|source| IntegrationError::Io {
                tool: tool.executable(),
                source,
            })?;
        if !output.status.success() {
            return Err(IntegrationError::Failed {
                tool: tool.executable(),
                status: output.status.to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        Ok(output)
    }
}

pub fn herdr_session_available() -> bool {
    env::var_os("HERDR_ENV").is_some_and(|value| value == "1")
}

pub fn missing_required_tools_with(
    mut available: impl FnMut(RequiredTool) -> bool,
) -> Vec<RequiredTool> {
    RequiredTool::ALL
        .into_iter()
        .filter(|tool| !available(*tool))
        .collect()
}

pub fn require_tools<R: BufRead, W: Write>(
    missing_tools: impl FnMut() -> Vec<RequiredTool>,
    input: &mut R,
    output: &mut W,
) -> io::Result<()> {
    let mut missing_tools = missing_tools;
    loop {
        let missing = missing_tools();
        if missing.is_empty() {
            return Ok(());
        }
        if !prompt_to_install(&missing, input, output)? {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "required tools are not installed",
            ));
        }
    }
}

pub fn prompt_to_install<R: BufRead, W: Write>(
    missing: &[RequiredTool],
    input: &mut R,
    output: &mut W,
) -> io::Result<bool> {
    if missing.is_empty() {
        return Ok(true);
    }

    for tool in missing {
        writeln!(
            output,
            "Required tool '{}' is missing; install it before starting the workflow.",
            tool.executable()
        )?;
    }
    write!(
        output,
        "Install the missing tools and press Enter to retry, or enter q to cancel: "
    )?;
    output.flush()?;

    let mut response = String::new();
    if input.read_line(&mut response)? == 0 {
        return Ok(false);
    }
    Ok(!response.trim().eq_ignore_ascii_case("q"))
}

fn valid_agent_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('a'..='z'))
        && name.len() <= 32
        && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '_' | '-'))
}

#[cfg(all(test, unix))]
mod tests {
    use super::{
        IntegrationError, RequiredTool, ToolCommands, missing_required_tools_with,
        prompt_to_install, require_tools,
    };
    use std::{ffi::OsStr, fs, io::Cursor, os::unix::fs::PermissionsExt, path::PathBuf};

    fn fake_command(directory: &std::path::Path, exit: i32) -> PathBuf {
        let path = directory.join(format!("fake-{exit}"));
        fs::write(
            &path,
            format!("#!/bin/sh\nprintf '<%s>\\n' \"$@\"\nexit {exit}\n"),
        )
        .unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&path, permissions).unwrap();
        path
    }

    fn commands(herdr: PathBuf) -> ToolCommands {
        ToolCommands::with_paths(herdr.clone(), herdr.clone(), herdr.clone(), herdr)
    }

    #[test]
    fn required_tool_prompt_lists_only_missing_tools_and_can_cancel() {
        let missing = missing_required_tools_with(|tool| tool != RequiredTool::Herdr);
        assert_eq!(missing, vec![RequiredTool::Herdr]);

        let mut input = Cursor::new(b"q\n".to_vec());
        let mut output = Vec::new();
        assert!(!prompt_to_install(&missing, &mut input, &mut output).unwrap());
        assert!(String::from_utf8(output).unwrap().contains("herdr"));
    }

    #[test]
    fn preflight_rechecks_tools_after_the_user_installs_them() {
        let mut checks = 0;
        let mut input = Cursor::new(b"\n".to_vec());
        let mut output = Vec::new();
        require_tools(
            || {
                checks += 1;
                if checks == 1 {
                    vec![RequiredTool::Pi]
                } else {
                    Vec::new()
                }
            },
            &mut input,
            &mut output,
        )
        .unwrap();
        assert!(
            String::from_utf8(output)
                .unwrap()
                .contains("Install the missing tools")
        );
    }

    #[test]
    fn herdr_arguments_are_separate_and_agent_failures_are_reported() {
        let temp = tempfile::tempdir().unwrap();
        let fake = fake_command(temp.path(), 0);
        let output = commands(fake.clone())
            .herdr_prompt_agent("reviewer-1", "review; do not execute shell syntax")
            .unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.contains("<review; do not execute shell syntax>"));
        let started = commands(fake.clone())
            .herdr_start_pi_agent("reviewer-1", "pane-1")
            .unwrap();
        assert!(
            String::from_utf8(started.stdout)
                .unwrap()
                .contains("<--kind>")
        );
        let read = commands(fake.clone())
            .herdr_read_agent("reviewer-1", 12)
            .unwrap();
        assert!(
            String::from_utf8(read.stdout)
                .unwrap()
                .contains("<recent-unwrapped>")
        );
        let canceled = commands(fake.clone())
            .herdr_cancel_agent("reviewer-1")
            .unwrap();
        let cancel_output = String::from_utf8(canceled.stdout).unwrap();
        assert!(cancel_output.contains("<wait>"));
        assert!(cancel_output.contains("<15000>"));

        let failing = commands(fake_command(temp.path(), 17));
        assert!(matches!(
            failing.herdr_cancel_agent("reviewer-1"),
            Err(IntegrationError::Failed { .. })
        ));
        assert!(matches!(
            commands(fake).herdr_cancel_agent("Bad Name"),
            Err(IntegrationError::InvalidAgentName)
        ));
    }

    #[test]
    fn openspec_and_git_adapters_preserve_argument_boundaries() {
        let temp = tempfile::tempdir().unwrap();
        let fake = fake_command(temp.path(), 0);
        let commands = commands(fake);
        let openspec = commands
            .run_openspec(&[OsStr::new("validate"), OsStr::new("change name; no shell")])
            .unwrap();
        assert!(
            String::from_utf8(openspec.stdout)
                .unwrap()
                .contains("<change name; no shell>")
        );
        let git = commands
            .run_git(&[OsStr::new("status"), OsStr::new("--short")])
            .unwrap();
        assert!(String::from_utf8(git.stdout).unwrap().contains("<--short>"));
    }

    #[test]
    fn pi_rpc_command_uses_project_extension_without_shell() {
        let temp = tempfile::tempdir().unwrap();
        let pi = PathBuf::from("pi");
        let commands = ToolCommands::with_paths(
            pi.clone(),
            PathBuf::from("herdr"),
            PathBuf::from("openspec"),
            PathBuf::from("git"),
        );
        let command = commands
            .pi_rpc_command(
                PathBuf::from("/project/extensions/workflow.ts").as_path(),
                temp.path(),
                PathBuf::from("/tmp/cronos data").as_path(),
                PathBuf::from("/usr/bin/cronos-ai").as_path(),
            )
            .unwrap();
        assert_eq!(command.get_program(), OsStr::new("pi"));
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                OsStr::new("--mode"),
                OsStr::new("rpc"),
                OsStr::new("--no-session"),
                OsStr::new("--extension"),
                OsStr::new("/project/extensions/workflow.ts"),
            ]
        );
        assert_eq!(command.get_current_dir(), Some(temp.path()));
        assert_eq!(command.get_envs().count(), 2);
    }
}
