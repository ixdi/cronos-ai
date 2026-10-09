use std::{
    env,
    ffi::{OsStr, OsString},
    io::{self, BufRead, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::mpsc,
    thread,
};

use thiserror::Error;

use crate::model::{ProjectId, TaskId};

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
    #[error("{0} path is not valid UTF-8")]
    NonUtf8Path(&'static str),
    #[error("Herdr returned an invalid pane-split response")]
    InvalidHerdrResponse,
}

enum PiRpcInput {
    Abort,
    Shutdown,
}

pub struct PiRpcHandle {
    pub events: mpsc::Receiver<String>,
    pub finished: mpsc::Receiver<Result<(), String>>,
    input: mpsc::Sender<PiRpcInput>,
}

impl PiRpcHandle {
    pub fn abort(&self) {
        let _ = self.input.send(PiRpcInput::Abort);
    }
}

#[derive(Debug, Error)]
pub enum GitError {
    #[error(transparent)]
    Integration(#[from] IntegrationError),
    #[error("not a Git repository: {0}")]
    InvalidRepository(String),
    #[error("worktree path must be absolute")]
    RelativeWorktreePath,
    #[error("worktree target already exists: {0}")]
    WorktreeExists(String),
    #[error("Git branch name is invalid")]
    InvalidBranch,
    #[error("Git repository is not clean: {0}")]
    DirtyRepository(String),
    #[error("local merge conflicted: {0}")]
    MergeConflict(String),
}

#[derive(Clone, Debug, Default)]
pub struct GitAdapter {
    tools: ToolCommands,
}

impl GitAdapter {
    pub fn with_tools(tools: ToolCommands) -> Self {
        Self { tools }
    }

    pub fn repository_root(&self, path: &Path) -> Result<PathBuf, GitError> {
        let canonical = std::fs::canonicalize(path).map_err(|source| IntegrationError::Io {
            tool: "git",
            source,
        })?;
        if !canonical.is_dir() {
            return Err(GitError::InvalidRepository(canonical.display().to_string()));
        }
        let output = self.tools.run_git(&[
            OsStr::new("-C"),
            canonical.as_os_str(),
            OsStr::new("rev-parse"),
            OsStr::new("--show-toplevel"),
        ])?;
        let root = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
        let root = std::fs::canonicalize(root).map_err(|source| IntegrationError::Io {
            tool: "git",
            source,
        })?;
        Ok(root)
    }

    pub fn worktree_branch(task_id: TaskId, attempt: u32) -> String {
        format!("cronos-ai/{}/attempt-{attempt}", task_id.0.simple())
    }

    pub fn add_worktree(
        &self,
        repository: &Path,
        worktree: &Path,
        branch: &str,
    ) -> Result<(), GitError> {
        let repository = self.repository_root(repository)?;
        if !worktree.is_absolute() {
            return Err(GitError::RelativeWorktreePath);
        }
        if worktree.exists() || std::fs::symlink_metadata(worktree).is_ok() {
            return Err(GitError::WorktreeExists(worktree.display().to_string()));
        }
        if !valid_branch_name(branch) {
            return Err(GitError::InvalidBranch);
        }
        self.tools.run_git(&[
            OsStr::new("-C"),
            repository.as_os_str(),
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("-b"),
            OsStr::new(branch),
            worktree.as_os_str(),
            OsStr::new("HEAD"),
        ])?;
        Ok(())
    }

    pub fn discard_attempt(
        &self,
        repository: &Path,
        worktree: &Path,
        branch: &str,
    ) -> Result<(), GitError> {
        if !valid_branch_name(branch) {
            return Err(GitError::InvalidBranch);
        }
        self.remove_worktree(repository, worktree, true)?;
        let repository = self.repository_root(repository)?;
        self.tools.run_git(&[
            OsStr::new("-C"),
            repository.as_os_str(),
            OsStr::new("branch"),
            OsStr::new("-D"),
            OsStr::new(branch),
        ])?;
        Ok(())
    }

    pub fn remove_worktree(
        &self,
        repository: &Path,
        worktree: &Path,
        force: bool,
    ) -> Result<(), GitError> {
        let repository = self.repository_root(repository)?;
        if !worktree.is_absolute() {
            return Err(GitError::RelativeWorktreePath);
        }
        let mut args = vec![
            OsStr::new("-C"),
            repository.as_os_str(),
            OsStr::new("worktree"),
            OsStr::new("remove"),
        ];
        if force {
            args.push(OsStr::new("--force"));
        }
        args.push(worktree.as_os_str());
        self.tools.run_git(&args)?;
        Ok(())
    }

    pub fn merge_worktree(
        &self,
        repository: &Path,
        worktree: &Path,
        branch: &str,
    ) -> Result<(), GitError> {
        let repository = self.repository_root(repository)?;
        if !worktree.is_absolute() {
            return Err(GitError::RelativeWorktreePath);
        }
        if !valid_branch_name(branch) {
            return Err(GitError::InvalidBranch);
        }
        self.ensure_clean(&repository)?;
        self.ensure_clean(worktree)?;
        match self.tools.run_git(&[
            OsStr::new("-C"),
            repository.as_os_str(),
            OsStr::new("merge"),
            OsStr::new("--no-ff"),
            OsStr::new("--no-edit"),
            OsStr::new(branch),
        ]) {
            Ok(_) => Ok(()),
            Err(IntegrationError::Failed { stderr, .. }) => {
                let abort = self.tools.run_git(&[
                    OsStr::new("-C"),
                    repository.as_os_str(),
                    OsStr::new("merge"),
                    OsStr::new("--abort"),
                ]);
                let detail = match abort {
                    Ok(_) => stderr,
                    Err(abort_error) => format!("{stderr}; merge abort failed: {abort_error}"),
                };
                Err(GitError::MergeConflict(detail))
            }
            Err(error) => Err(error.into()),
        }
    }

    fn ensure_clean(&self, path: &Path) -> Result<(), GitError> {
        let output = self.tools.run_git(&[
            OsStr::new("-C"),
            path.as_os_str(),
            OsStr::new("status"),
            OsStr::new("--porcelain"),
        ])?;
        let status = String::from_utf8_lossy(&output.stdout);
        if status.trim().is_empty() {
            Ok(())
        } else {
            Err(GitError::DirtyRepository(path.display().to_string()))
        }
    }
}

fn valid_branch_name(branch: &str) -> bool {
    !branch.is_empty()
        && branch.starts_with("cronos-ai/")
        && branch.chars().all(|ch| {
            ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '-' | '_' | '/')
        })
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
    pub fn spawn_pi_rpc(&self, mut command: Command) -> Result<PiRpcHandle, IntegrationError> {
        let mut child = command
            .spawn()
            .map_err(|source| IntegrationError::Io { tool: "pi", source })?;
        let stdin = child.stdin.take().ok_or_else(|| IntegrationError::Io {
            tool: "pi",
            source: io::Error::new(io::ErrorKind::BrokenPipe, "Pi stdin was not piped"),
        })?;
        let stdout = child.stdout.take().ok_or_else(|| IntegrationError::Io {
            tool: "pi",
            source: io::Error::new(io::ErrorKind::BrokenPipe, "Pi stdout was not piped"),
        })?;
        let stderr = child.stderr.take().ok_or_else(|| IntegrationError::Io {
            tool: "pi",
            source: io::Error::new(io::ErrorKind::BrokenPipe, "Pi stderr was not piped"),
        })?;
        let (input, input_rx) = mpsc::channel();
        let (events_tx, events) = mpsc::channel();
        let (finished_tx, finished) = mpsc::channel();
        let input_from_reader = input.clone();

        thread::spawn(move || {
            let writer = thread::spawn(move || {
                let mut stdin = BufWriter::new(stdin);
                while let Ok(message) = input_rx.recv() {
                    let command = match message {
                        PiRpcInput::Abort => r#"{"type":"abort"}"#,
                        PiRpcInput::Shutdown => break,
                    };
                    if writeln!(stdin, "{command}")
                        .and_then(|()| stdin.flush())
                        .is_err()
                    {
                        break;
                    }
                }
            });
            let stderr_reader = thread::spawn(move || {
                let mut contents = String::new();
                let _ = BufReader::new(stderr).read_to_string(&mut contents);
                contents
            });

            let mut settled = false;
            for line in BufReader::new(stdout).lines() {
                let line = match line {
                    Ok(line) => line,
                    Err(error) => {
                        let _ = events_tx.send(format!("Pi RPC stdout error: {error}"));
                        break;
                    }
                };
                let Ok(record) = serde_json::from_str::<serde_json::Value>(&line) else {
                    let _ = events_tx.send("Pi RPC emitted invalid JSON".to_owned());
                    continue;
                };
                match record.get("type").and_then(serde_json::Value::as_str) {
                    Some("agent_settled") => {
                        settled = true;
                        let _ = events_tx.send("Pi workflow settled".to_owned());
                        let _ = input_from_reader.send(PiRpcInput::Shutdown);
                        break;
                    }
                    Some("extension_error") => {
                        let message = record
                            .get("error")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("Pi extension error");
                        let _ = events_tx.send(message.to_owned());
                    }
                    Some("response")
                        if record.get("success") == Some(&serde_json::Value::Bool(false)) =>
                    {
                        let message = record
                            .get("error")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("Pi RPC command failed");
                        let _ = events_tx.send(message.to_owned());
                    }
                    _ => {}
                }
            }
            let _ = input_from_reader.send(PiRpcInput::Shutdown);
            let _ = writer.join();
            let stderr = stderr_reader.join().unwrap_or_default();
            let result = match child.wait() {
                Ok(status) if status.success() => Ok(()),
                Ok(status) => Err(format!("Pi exited with {status}: {}", stderr.trim())),
                Err(error) => Err(format!("could not wait for Pi: {error}")),
            };
            if !settled && result.is_ok() {
                let _ = events_tx.send("Pi exited before the workflow settled".to_owned());
            }
            let _ = finished_tx.send(result);
        });

        Ok(PiRpcHandle {
            events,
            finished,
            input,
        })
    }

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
        project_id: ProjectId,
        task_id: TaskId,
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
                OsStr::new("--no-extensions"),
                OsStr::new("--no-builtin-tools"),
                OsStr::new("--no-mcp"),
                OsStr::new("--no-skills"),
                OsStr::new("--no-context-files"),
            ])
            .arg("--extension")
            .arg(extension)
            .current_dir(worktree)
            .env("CRONOS_AI_BIN", executable_path)
            .env("CRONOS_AI_DATA_DIR", data_dir)
            .env("CRONOS_PROJECT_ID", project_id.0.to_string())
            .env("CRONOS_TASK_ID", task_id.0.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        Ok(command)
    }

    pub fn herdr_split_pane(&self, worktree: &Path) -> Result<String, IntegrationError> {
        if !worktree.is_absolute() {
            return Err(IntegrationError::RelativePath("worktree"));
        }
        let output = self.run(
            RequiredTool::Herdr,
            &[
                OsStr::new("pane"),
                OsStr::new("split"),
                OsStr::new("--current"),
                OsStr::new("--direction"),
                OsStr::new("right"),
                OsStr::new("--cwd"),
                worktree.as_os_str(),
                OsStr::new("--no-focus"),
            ],
        )?;
        serde_json::from_slice::<serde_json::Value>(&output.stdout)
            .ok()
            .and_then(|response| {
                response
                    .pointer("/result/pane/pane_id")?
                    .as_str()
                    .map(str::to_owned)
            })
            .ok_or(IntegrationError::InvalidHerdrResponse)
    }

    pub fn herdr_create_workspace(
        &self,
        session: &str,
        cwd: &Path,
    ) -> Result<String, IntegrationError> {
        if !cwd.is_absolute() {
            return Err(IntegrationError::RelativePath("working directory"));
        }
        let output = self.run(
            RequiredTool::Herdr,
            &[
                OsStr::new("--session"),
                OsStr::new(session),
                OsStr::new("workspace"),
                OsStr::new("create"),
                OsStr::new("--cwd"),
                cwd.as_os_str(),
                OsStr::new("--label"),
                OsStr::new("Cronos-AI"),
                OsStr::new("--focus"),
            ],
        )?;
        serde_json::from_slice::<serde_json::Value>(&output.stdout)
            .ok()
            .and_then(|response| {
                response
                    .pointer("/result/root_pane/pane_id")?
                    .as_str()
                    .filter(|pane_id| !pane_id.is_empty())
                    .map(str::to_owned)
            })
            .ok_or(IntegrationError::InvalidHerdrResponse)
    }

    pub fn herdr_attach_session(&self, session: &str) -> Result<(), IntegrationError> {
        let status = Command::new(&self.herdr)
            .arg("--session")
            .arg(session)
            .status()
            .map_err(|source| IntegrationError::Io {
                tool: RequiredTool::Herdr.executable(),
                source,
            })?;
        if status.success() {
            Ok(())
        } else {
            Err(IntegrationError::Failed {
                tool: RequiredTool::Herdr.executable(),
                status: status.to_string(),
                stderr: String::new(),
            })
        }
    }

    pub fn herdr_run_tui(
        &self,
        session: &str,
        pane_id: &str,
        executable: &Path,
    ) -> Result<Output, IntegrationError> {
        if !executable.is_absolute() {
            return Err(IntegrationError::RelativePath("Cronos executable"));
        }
        if pane_id.is_empty() {
            return Err(IntegrationError::InvalidHerdrResponse);
        }
        let executable = executable
            .to_str()
            .ok_or(IntegrationError::NonUtf8Path("Cronos executable"))?;
        let command = quote_shell_word(executable);
        self.run(
            RequiredTool::Herdr,
            &[
                OsStr::new("--session"),
                OsStr::new(session),
                OsStr::new("pane"),
                OsStr::new("run"),
                OsStr::new(pane_id),
                OsStr::new(&command),
            ],
        )
    }

    pub fn herdr_start_pi_agent(
        &self,
        name: &str,
        pane_id: &str,
        pi_args: &[OsString],
    ) -> Result<Output, IntegrationError> {
        if !valid_agent_name(name) {
            return Err(IntegrationError::InvalidAgentName);
        }
        let mut args = [
            "agent", "start", name, "--kind", "pi", "--pane", pane_id, "--",
        ]
        .into_iter()
        .map(OsString::from)
        .collect::<Vec<_>>();
        args.extend_from_slice(pi_args);
        let args = args.iter().map(OsString::as_os_str).collect::<Vec<_>>();
        self.run(RequiredTool::Herdr, &args)
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

    pub fn herdr_agent_status(&self, name: &str) -> Result<String, IntegrationError> {
        if !valid_agent_name(name) {
            return Err(IntegrationError::InvalidAgentName);
        }
        let output = self.run(
            RequiredTool::Herdr,
            &[OsStr::new("agent"), OsStr::new("get"), OsStr::new(name)],
        )?;
        let response: serde_json::Value = serde_json::from_slice(&output.stdout)
            .map_err(|_| IntegrationError::InvalidHerdrResponse)?;
        find_herdr_status(&response)
            .map(str::to_owned)
            .ok_or(IntegrationError::InvalidHerdrResponse)
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

fn find_herdr_status(value: &serde_json::Value) -> Option<&str> {
    match value {
        serde_json::Value::Object(object) => {
            if let Some(status) = object.get("status").and_then(serde_json::Value::as_str)
                && matches!(status, "idle" | "working" | "blocked" | "done" | "unknown")
            {
                return Some(status);
            }
            object.values().find_map(find_herdr_status)
        }
        serde_json::Value::Array(items) => items.iter().find_map(find_herdr_status),
        _ => None,
    }
}

fn valid_agent_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('a'..='z'))
        && name.len() <= 32
        && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '_' | '-'))
}

fn quote_shell_word(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::{
        GitAdapter, GitError, IntegrationError, RequiredTool, ToolCommands,
        missing_required_tools_with, prompt_to_install, quote_shell_word, require_tools,
    };
    use std::{
        ffi::{OsStr, OsString},
        fs,
        io::Cursor,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
        process::Command,
    };

    fn fake_json_command(directory: &std::path::Path) -> PathBuf {
        let path = directory.join("fake-json");
        fs::write(
            &path,
            "#!/bin/sh\nprintf '%s' '{\"result\":{\"pane\":{\"pane_id\":\"w1:p9\"}}}'\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&path, permissions).unwrap();
        path
    }

    fn fake_workspace_command(directory: &std::path::Path, response: &str) -> PathBuf {
        let path = directory.join("fake-workspace");
        fs::write(
            &path,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$(dirname \"$0\")/args\"\nprintf '%s' '{response}'\n"
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&path, permissions).unwrap();
        path
    }

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
        let pi_args = vec![
            OsString::from("--no-skills"),
            OsString::from("--no-extensions"),
            OsString::from("--no-context-files"),
            OsString::from("--no-mcp"),
            OsString::from("--skill"),
            OsString::from("/path with spaces/security/SKILL.md"),
            OsString::from("--extension"),
            OsString::from("/path with spaces/pi-mcp-adapter"),
            OsString::from("--mcp-config"),
            OsString::from("/path with spaces/security-mcp.json"),
        ];
        let started = commands(fake.clone())
            .herdr_start_pi_agent("reviewer-1", "pane-1", &pi_args)
            .unwrap();
        let started_output = String::from_utf8(started.stdout).unwrap();
        assert!(started_output.contains("<--kind>"));
        assert!(started_output.contains("<-->"));
        assert!(started_output.contains("<--no-skills>"));
        assert!(started_output.contains("<--no-extensions>"));
        assert!(started_output.contains("<--no-context-files>"));
        assert!(started_output.contains("<--no-mcp>"));
        assert!(started_output.contains("<--skill>"));
        assert!(started_output.contains("</path with spaces/security/SKILL.md>"));
        assert!(started_output.contains("<--extension>"));
        assert!(started_output.contains("</path with spaces/pi-mcp-adapter>"));
        assert!(started_output.contains("<--mcp-config>"));
        assert!(started_output.contains("</path with spaces/security-mcp.json>"));
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
    fn herdr_launcher_adapter_preserves_paths_and_quotes_tui_executable() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("project with spaces");
        fs::create_dir(&cwd).unwrap();
        let executable = temp.path().join("cronos' ai");
        fs::write(&executable, "").unwrap();
        let fake = fake_workspace_command(
            temp.path(),
            r#"{"result":{"root_pane":{"pane_id":"w5:p8"}}}"#,
        );
        let tools = commands(fake);

        let pane = tools.herdr_create_workspace("cronos-ai", &cwd).unwrap();
        assert_eq!(pane, "w5:p8");
        tools
            .herdr_run_tui("cronos-ai", &pane, &executable)
            .unwrap();

        let args = fs::read_to_string(temp.path().join("args")).unwrap();
        let args: Vec<_> = args.lines().collect();
        assert_eq!(
            &args[..9],
            [
                "--session",
                "cronos-ai",
                "workspace",
                "create",
                "--cwd",
                cwd.to_str().unwrap(),
                "--label",
                "Cronos-AI",
                "--focus",
            ]
        );
        assert_eq!(
            &args[9..],
            [
                "--session",
                "cronos-ai",
                "pane",
                "run",
                "w5:p8",
                quote_shell_word(executable.to_str().unwrap()).as_str(),
            ]
        );
    }

    #[test]
    fn herdr_launcher_adapter_rejects_invalid_workspace_response() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let fake = fake_workspace_command(temp.path(), r#"{"result":{"workspace":{}}}"#);
        assert!(matches!(
            commands(fake).herdr_create_workspace("cronos-ai", &cwd),
            Err(IntegrationError::InvalidHerdrResponse)
        ));
    }

    #[test]
    fn herdr_split_adapter_extracts_server_assigned_pane_id() {
        let temp = tempfile::tempdir().unwrap();
        let commands = commands(fake_json_command(temp.path()));
        assert_eq!(commands.herdr_split_pane(temp.path()).unwrap(), "w1:p9");
        assert!(matches!(
            commands.herdr_split_pane(Path::new("relative")),
            Err(IntegrationError::RelativePath("worktree"))
        ));
    }

    #[test]
    fn herdr_status_adapter_recognizes_blocked_state() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("fake-status");
        fs::write(
            &path,
            "#!/bin/sh\nprintf '%s' '{\"result\":{\"agent\":{\"status\":\"blocked\"}}}'\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&path, permissions).unwrap();
        assert_eq!(
            commands(path).herdr_agent_status("reviewer").unwrap(),
            "blocked"
        );
    }

    fn fixture_git(root: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn init_git_fixture(root: &Path) {
        fs::create_dir_all(root).unwrap();
        fixture_git(root, &["init"]);
        fixture_git(root, &["config", "user.name", "Cronos Tests"]);
        fixture_git(root, &["config", "user.email", "cronos@example.invalid"]);
        fs::write(root.join("tracked.txt"), "base\n").unwrap();
        fixture_git(root, &["add", "tracked.txt"]);
        fixture_git(root, &["commit", "-m", "initial"]);
    }

    fn recording_git_adapter(directory: &Path) -> GitAdapter {
        let log = directory.join("git-calls.log");
        let wrapper = directory.join("git-wrapper");
        fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexec git \"$@\"\n",
                log.display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&wrapper).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&wrapper, permissions).unwrap();
        GitAdapter::with_tools(ToolCommands {
            git: wrapper,
            ..ToolCommands::default()
        })
    }

    #[test]
    fn git_worktree_is_locally_merged_without_remote_push() {
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repo");
        let worktree = temp.path().join("worktrees/task/attempt-1");
        init_git_fixture(&repository);
        fs::create_dir_all(worktree.parent().unwrap()).unwrap();
        let adapter = recording_git_adapter(temp.path());
        let branch = "cronos-ai/task/attempt-1";
        adapter
            .add_worktree(&repository, &worktree, branch)
            .unwrap();
        fs::write(worktree.join("generated.txt"), "task output\n").unwrap();
        fixture_git(&worktree, &["add", "generated.txt"]);
        fixture_git(&worktree, &["commit", "-m", "task output"]);
        adapter
            .merge_worktree(&repository, &worktree, branch)
            .unwrap();
        assert_eq!(
            fs::read_to_string(repository.join("generated.txt")).unwrap(),
            "task output\n"
        );
        let log = fs::read_to_string(temp.path().join("git-calls.log")).unwrap();
        assert!(
            !log.lines()
                .any(|line| line.split_whitespace().any(|token| token == "push"))
        );
    }

    #[test]
    fn merge_conflict_is_aborted_without_changing_the_base_worktree() {
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repo");
        let worktree = temp.path().join("worktrees/task/attempt-1");
        init_git_fixture(&repository);
        fs::create_dir_all(worktree.parent().unwrap()).unwrap();
        let adapter = recording_git_adapter(temp.path());
        let branch = "cronos-ai/task/attempt-1";
        adapter
            .add_worktree(&repository, &worktree, branch)
            .unwrap();
        fs::write(worktree.join("tracked.txt"), "agent change\n").unwrap();
        fixture_git(&worktree, &["add", "tracked.txt"]);
        fixture_git(&worktree, &["commit", "-m", "agent change"]);
        fs::write(repository.join("tracked.txt"), "human change\n").unwrap();
        fixture_git(&repository, &["add", "tracked.txt"]);
        fixture_git(&repository, &["commit", "-m", "human change"]);

        assert!(matches!(
            adapter.merge_worktree(&repository, &worktree, branch),
            Err(GitError::MergeConflict(_))
        ));
        assert_eq!(
            fs::read_to_string(repository.join("tracked.txt")).unwrap(),
            "human change\n"
        );
        assert!(fixture_git(&repository, &["status", "--porcelain"]).is_empty());
        assert_eq!(
            fs::read_to_string(worktree.join("tracked.txt")).unwrap(),
            "agent change\n"
        );
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
    fn pi_rpc_host_drains_events_until_the_agent_settles() {
        let temp = tempfile::tempdir().unwrap();
        let fake = temp.path().join("fake-pi");
        fs::write(
            &fake,
            "#!/bin/sh\nprintf '%s\\n' '{\"type\":\"agent_settled\"}'\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&fake).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&fake, permissions).unwrap();
        let tools = ToolCommands::with_paths(
            fake,
            PathBuf::from("herdr"),
            PathBuf::from("openspec"),
            PathBuf::from("git"),
        );
        let command = tools
            .pi_rpc_command(
                Path::new("/extension.ts"),
                temp.path(),
                temp.path(),
                Path::new("/cronos-ai"),
                crate::model::ProjectId::new(),
                crate::model::TaskId::new(),
            )
            .unwrap();
        let handle = tools.spawn_pi_rpc(command).unwrap();
        assert_eq!(
            handle
                .events
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap(),
            "Pi workflow settled"
        );
        assert!(
            handle
                .finished
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap()
                .is_ok()
        );
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
                crate::model::ProjectId::new(),
                crate::model::TaskId::new(),
            )
            .unwrap();
        assert_eq!(command.get_program(), OsStr::new("pi"));
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                OsStr::new("--mode"),
                OsStr::new("rpc"),
                OsStr::new("--no-session"),
                OsStr::new("--no-extensions"),
                OsStr::new("--no-builtin-tools"),
                OsStr::new("--no-mcp"),
                OsStr::new("--no-skills"),
                OsStr::new("--no-context-files"),
                OsStr::new("--extension"),
                OsStr::new("/project/extensions/workflow.ts"),
            ]
        );
        assert_eq!(command.get_current_dir(), Some(temp.path()));
        assert_eq!(command.get_envs().count(), 4);
    }
}
