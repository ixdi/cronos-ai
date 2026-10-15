use std::{
    env,
    ffi::{OsStr, OsString},
    io::{self, BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use thiserror::Error;

use crate::release::{Commit, Version};

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

    /// A handle driven by the test instead of a Pi process: send events and the final result
    /// through the returned senders.
    #[cfg(test)]
    pub(crate) fn for_tests() -> (Self, mpsc::Sender<String>, mpsc::Sender<Result<(), String>>) {
        let (event_sender, events) = mpsc::channel();
        let (finished_sender, finished) = mpsc::channel();
        let (input, _) = mpsc::channel();
        (
            Self {
                events,
                finished,
                input,
            },
            event_sender,
            finished_sender,
        )
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
    #[error("base repository has uncommitted changes: {0}")]
    DirtyBase(String),
    #[error("local merge conflicted: {0}")]
    MergeConflict(String),
    #[error("local merge failed: {0}")]
    MergeFailed(String),
    #[error("push failed: {0}")]
    PushFailed(String),
    #[error("could not open a pull request: {0}")]
    PullRequestFailed(String),
    #[error("could not create the release: {0}")]
    ReleaseFailed(String),
}

/// A pull request from a pushed task branch into the base branch.
pub struct PullRequest<'a> {
    pub remote: &'a str,
    pub base: &'a str,
    pub head: &'a str,
    pub title: &'a str,
    pub body: &'a str,
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
        self.merge_branch(repository, worktree, branch, None)
    }

    /// Merges a parallel writer's branch into the task branch with a message naming the agent.
    pub fn merge_agent(
        &self,
        task_worktree: &Path,
        agent_worktree: &Path,
        branch: &str,
        message: &str,
    ) -> Result<(), GitError> {
        self.merge_branch(task_worktree, agent_worktree, branch, Some(message))
    }

    /// Moves the checked-out branch back to `commit`, discarding later commits and changes.
    pub fn reset_hard(&self, worktree: &Path, commit: &str) -> Result<(), GitError> {
        if commit.is_empty() || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(GitError::InvalidBranch);
        }
        self.git_text(worktree, &["reset", "--hard", commit])?;
        Ok(())
    }

    fn merge_branch(
        &self,
        repository: &Path,
        worktree: &Path,
        branch: &str,
        message: Option<&str>,
    ) -> Result<(), GitError> {
        let repository = self.repository_root(repository)?;
        if !worktree.is_absolute() {
            return Err(GitError::RelativeWorktreePath);
        }
        if !valid_branch_name(branch) {
            return Err(GitError::InvalidBranch);
        }
        if !self.is_clean(&repository)? {
            return Err(GitError::DirtyBase(repository.display().to_string()));
        }
        self.ensure_clean(worktree)?;
        let mut args = vec![
            OsStr::new("-C"),
            repository.as_os_str(),
            OsStr::new("merge"),
            OsStr::new("--no-ff"),
            OsStr::new("--no-edit"),
        ];
        if let Some(message) = message {
            args.extend([OsStr::new("-m"), OsStr::new(message)]);
        }
        args.push(OsStr::new(branch));
        match self.tools.run_git(&args) {
            Ok(_) => Ok(()),
            Err(IntegrationError::Failed { stderr, .. }) => {
                // Only unmerged paths are a conflict the code-generation agent can resolve;
                // anything else (for example untracked files in the way) needs a human.
                let conflicted = !self
                    .git_text(&repository, &["diff", "--name-only", "--diff-filter=U"])
                    .unwrap_or_default()
                    .trim()
                    .is_empty();
                let merging = self
                    .git_text(&repository, &["rev-parse", "-q", "--verify", "MERGE_HEAD"])
                    .is_ok();
                let abort = if merging {
                    self.tools
                        .run_git(&[
                            OsStr::new("-C"),
                            repository.as_os_str(),
                            OsStr::new("merge"),
                            OsStr::new("--abort"),
                        ])
                        .map(|_| ())
                } else {
                    Ok(())
                };
                let detail = match abort {
                    Ok(()) => stderr,
                    Err(abort_error) => format!("{stderr}; merge abort failed: {abort_error}"),
                };
                if conflicted {
                    Err(GitError::MergeConflict(detail))
                } else {
                    Err(GitError::MergeFailed(detail))
                }
            }
            Err(error) => Err(error.into()),
        }
    }

    fn ensure_clean(&self, path: &Path) -> Result<(), GitError> {
        if self.is_clean(path)? {
            Ok(())
        } else {
            Err(GitError::DirtyRepository(path.display().to_string()))
        }
    }

    fn git_text(&self, path: &Path, args: &[&str]) -> Result<String, GitError> {
        let mut full = vec![OsStr::new("-C"), path.as_os_str()];
        full.extend(args.iter().map(OsStr::new));
        let output = self.tools.run_git(&full)?;
        Ok(String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_owned())
    }

    pub fn status(&self, path: &Path) -> Result<String, GitError> {
        self.git_text(path, &["status", "--porcelain"])
    }

    pub fn is_clean(&self, path: &Path) -> Result<bool, GitError> {
        Ok(self.status(path)?.trim().is_empty())
    }

    pub fn head(&self, path: &Path) -> Result<String, GitError> {
        self.git_text(path, &["rev-parse", "HEAD"])
    }

    /// Whether `branch` is already part of the repository's checked-out branch.
    pub fn is_merged(&self, repository: &Path, branch: &str) -> Result<bool, GitError> {
        if !valid_branch_name(branch) {
            return Err(GitError::InvalidBranch);
        }
        match self.tools.run_git(&[
            OsStr::new("-C"),
            repository.as_os_str(),
            OsStr::new("merge-base"),
            OsStr::new("--is-ancestor"),
            OsStr::new(branch),
            OsStr::new("HEAD"),
        ]) {
            Ok(_) => Ok(true),
            // Exit status 1 means "not an ancestor"; other failures carry an error message.
            Err(IntegrationError::Failed { stderr, .. }) if stderr.is_empty() => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    /// Pushes `branch` from `path` to the same branch name on `remote`.
    pub fn push(&self, path: &Path, remote: &str, branch: &str) -> Result<(), GitError> {
        let reference = format!("refs/heads/{branch}");
        // Any branch may be pushed (the base branch too), but only one Git itself accepts.
        if branch.starts_with('-')
            || self
                .tools
                .run_git(&[OsStr::new("check-ref-format"), OsStr::new(&reference)])
                .is_err()
        {
            return Err(GitError::InvalidBranch);
        }
        let refspec = format!("{reference}:{reference}");
        self.tools
            .run_git(&[
                OsStr::new("-C"),
                path.as_os_str(),
                OsStr::new("push"),
                OsStr::new("--"),
                OsStr::new(remote),
                OsStr::new(&refspec),
            ])
            .map(|_| ())
            .map_err(|error| GitError::PushFailed(format!("{remote}: {error}")))
    }

    /// Pushes the task branch and returns the URL of its open pull request, creating it only when
    /// none exists, so retrying never opens a duplicate.
    pub fn open_pull_request(
        &self,
        repository: &Path,
        worktree: &Path,
        request: &PullRequest<'_>,
    ) -> Result<String, GitError> {
        self.ensure_clean(worktree)?;
        self.push(worktree, request.remote, request.head)?;
        if let Ok(output) = self.tools.run_gh(
            repository,
            &["pr", "view", request.head, "--json", "url", "--jq", ".url"],
        ) && let Some(url) = last_line(&output.stdout)
        {
            return Ok(url);
        }
        let output = self
            .tools
            .run_gh(
                repository,
                &[
                    "pr",
                    "create",
                    "--base",
                    request.base,
                    "--head",
                    request.head,
                    "--title",
                    request.title,
                    "--body",
                    request.body,
                ],
            )
            .map_err(|error| GitError::PullRequestFailed(error.to_string()))?;
        last_line(&output.stdout)
            .ok_or_else(|| GitError::PullRequestFailed("gh printed no pull request URL".to_owned()))
    }

    /// The absolute Git directory shared by the repository and all its worktrees.
    pub fn common_dir(&self, path: &Path) -> Result<PathBuf, GitError> {
        self.git_text(
            path,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )
        .map(PathBuf::from)
    }

    pub fn current_branch(&self, path: &Path) -> Result<String, GitError> {
        self.git_text(path, &["rev-parse", "--abbrev-ref", "HEAD"])
    }

    /// Object ID of a committed file at HEAD, or None when HEAD has no such file.
    pub fn committed_blob(&self, path: &Path, file: &str) -> Option<String> {
        self.git_text(
            path,
            &["rev-parse", "-q", "--verify", &format!("HEAD:{file}")],
        )
        .ok()
    }

    /// Commit and file status, used to detect writes by read-only steps.
    pub fn snapshot(&self, path: &Path) -> Result<String, GitError> {
        Ok(format!("{}\n{}", self.head(path)?, self.status(path)?))
    }

    pub fn commit_all(&self, path: &Path, message: &str) -> Result<(), GitError> {
        self.git_text(path, &["add", "-A"])?;
        self.git_text(path, &["commit", "-m", message])?;
        Ok(())
    }

    /// Keep Cronos result files out of every worktree without touching tracked files.
    pub fn ensure_local_exclude(&self, repository: &Path, pattern: &str) -> Result<(), GitError> {
        let common = PathBuf::from(self.git_text(repository, &["rev-parse", "--git-common-dir"])?);
        let common = if common.is_absolute() {
            common
        } else {
            repository.join(common)
        };
        let info = common.join("info");
        let exclude = info.join("exclude");
        let current = std::fs::read_to_string(&exclude).unwrap_or_default();
        if current.lines().any(|line| line.trim() == pattern) {
            return Ok(());
        }
        let io_error = |source| IntegrationError::Io {
            tool: "git",
            source,
        };
        std::fs::create_dir_all(&info).map_err(io_error)?;
        let separator = if current.is_empty() || current.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        std::fs::write(&exclude, format!("{current}{separator}{pattern}\n")).map_err(io_error)?;
        Ok(())
    }

    /// Commits and diff summary of the task branch against the base repository's HEAD.
    pub fn review_summary(&self, repository: &Path, worktree: &Path) -> Result<String, GitError> {
        let base = self.head(repository)?;
        let log = self.git_text(worktree, &["log", "--oneline", &format!("{base}..HEAD")])?;
        let diff = self.git_text(worktree, &["diff", "--stat", &format!("{base}...HEAD")])?;
        Ok(format!(
            "Commits on the task branch:\n{}\n\nDiff summary against the base:\n{}",
            if log.is_empty() { "(none)" } else { &log },
            if diff.is_empty() {
                "(no changes)"
            } else {
                &diff
            },
        ))
    }

    /// The highest `vX.Y.Z` tag reachable from the repository's HEAD.
    pub fn latest_release(&self, repository: &Path) -> Result<Option<Version>, GitError> {
        let tags = self.git_text(repository, &["tag", "--merged", "HEAD", "--list", "v*"])?;
        Ok(tags.lines().filter_map(Version::parse).max())
    }

    /// The release tagged on HEAD, if any; lets an interrupted release be adopted, not repeated.
    pub fn release_at_head(&self, repository: &Path) -> Result<Option<Version>, GitError> {
        let tags = self.git_text(repository, &["tag", "--points-at", "HEAD", "--list", "v*"])?;
        Ok(tags.lines().filter_map(Version::parse).max())
    }

    /// Non-merge commits in `range` (for example `v1.2.0..HEAD`), newest first.
    pub fn commits(&self, repository: &Path, range: &str) -> Result<Vec<Commit>, GitError> {
        if range.starts_with('-') {
            return Err(GitError::InvalidBranch);
        }
        let log = self.git_text(
            repository,
            &[
                "log",
                "--no-merges",
                "--format=%H%x1f%s%x1f%b%x1e",
                range,
                "--",
            ],
        )?;
        Ok(log
            .split('\u{1e}')
            .filter_map(|record| {
                let mut fields = record.trim_start_matches('\n').splitn(3, '\u{1f}');
                Some(Commit {
                    hash: fields.next().filter(|hash| !hash.is_empty())?.to_owned(),
                    subject: fields.next()?.to_owned(),
                    body: fields.next().unwrap_or_default().trim().to_owned(),
                })
            })
            .collect())
    }

    /// Writes the changelog, commits it as `chore(release): <version>`, and tags that commit.
    pub fn commit_release(
        &self,
        repository: &Path,
        changelog: &str,
        content: &str,
        version: &Version,
        message: &str,
    ) -> Result<(), GitError> {
        let relative = Path::new(changelog);
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(GitError::ReleaseFailed(format!(
                "changelog path must stay inside the repository: {changelog}"
            )));
        }
        let path = repository.join(relative);
        if std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err(GitError::ReleaseFailed(format!(
                "{changelog} must not be a symlink"
            )));
        }
        std::fs::write(&path, content)
            .map_err(|error| GitError::ReleaseFailed(format!("{changelog}: {error}")))?;
        let tag = version.to_string();
        let subject = format!("chore(release): {tag}");
        self.git_text(repository, &["add", "--", changelog])?;
        self.git_text(repository, &["commit", "-m", &subject, "--", changelog])?;
        self.git_text(repository, &["tag", "-a", &tag, "-m", message])?;
        Ok(())
    }

    /// Pushes one tag to `remote`.
    pub fn push_tag(&self, path: &Path, remote: &str, tag: &str) -> Result<(), GitError> {
        if Version::parse(tag).is_none() {
            return Err(GitError::InvalidBranch);
        }
        let refspec = format!("refs/tags/{tag}:refs/tags/{tag}");
        self.tools
            .run_git(&[
                OsStr::new("-C"),
                path.as_os_str(),
                OsStr::new("push"),
                OsStr::new("--"),
                OsStr::new(remote),
                OsStr::new(&refspec),
            ])
            .map(|_| ())
            .map_err(|error| GitError::PushFailed(format!("{remote}: {error}")))
    }

    /// The task branch's full diff against the base, without color, cut after `max_lines` with a
    /// note saying how much was left out.
    pub fn review_diff(
        &self,
        repository: &Path,
        worktree: &Path,
        max_lines: usize,
    ) -> Result<String, GitError> {
        let base = self.head(repository)?;
        let diff = self.git_text(
            worktree,
            &[
                "diff",
                "--no-color",
                "--no-ext-diff",
                &format!("{base}...HEAD"),
            ],
        )?;
        let total = diff.lines().count();
        if total <= max_lines {
            return Ok(diff);
        }
        let mut shown = diff.lines().take(max_lines).collect::<Vec<_>>().join("\n");
        shown.push_str(&format!(
            "\n… {} more diff lines not shown; inspect the worktree for the rest.",
            total - max_lines
        ));
        Ok(shown)
    }
}

pub struct CheckOutcome {
    pub success: bool,
    pub output_tail: String,
}

/// Runs a project check command without a shell; output goes to `<worktree>/.cronos/check-output.log`.
pub fn run_check_command(
    worktree: &Path,
    command: &[String],
    timeout: Duration,
) -> Result<CheckOutcome, IntegrationError> {
    let io_error = |source| IntegrationError::Io {
        tool: "check command",
        source,
    };
    let (program, args) = command.split_first().ok_or(IntegrationError::Io {
        tool: "check command",
        source: io::Error::new(io::ErrorKind::InvalidInput, "check command is empty"),
    })?;
    let mut program = PathBuf::from(program);
    // Rust leaves relative program resolution to the platform; anchor paths like ./check.sh to the worktree.
    if program.is_relative() && program.components().count() > 1 {
        program = worktree.join(program);
    }
    let log_dir = ensure_real_directory(&worktree.join(".cronos")).map_err(io_error)?;
    let log_path = log_dir.join("check-output.log");
    let log = create_fresh_file(&log_path).map_err(io_error)?;
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(worktree)
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(io_error)?)
        .stderr(log.try_clone().map_err(io_error)?);
    // Own process group, so a timeout also stops the processes the check starts.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = command.spawn().map_err(io_error)?;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(io_error)? {
            break Some(status);
        }
        if started.elapsed() >= timeout {
            #[cfg(unix)]
            let _ = Command::new("kill")
                .args(["-KILL", &format!("-{}", child.id())])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        thread::sleep(Duration::from_millis(100));
    };
    // Read the tail through the handle we created, never by path.
    let mut log = log;
    let length = log.seek(SeekFrom::End(0)).map_err(io_error)?;
    log.seek(SeekFrom::Start(length.saturating_sub(MAX_CHECK_TAIL_BYTES)))
        .map_err(io_error)?;
    let mut output = Vec::new();
    log.read_to_end(&mut output).map_err(io_error)?;
    let output = String::from_utf8_lossy(&output);
    let lines = output.lines().collect::<Vec<_>>();
    let mut output_tail = lines[lines.len().saturating_sub(40)..].join("\n");
    if status.is_none() {
        output_tail.push_str(&format!(
            "\ncheck command exceeded its {}-minute limit",
            timeout.as_secs() / 60
        ));
    }
    Ok(CheckOutcome {
        success: status.is_some_and(|status| status.success()),
        output_tail,
    })
}

const MAX_CHECK_TAIL_BYTES: u64 = 256 * 1024;

/// Creates `path` as a directory if needed and refuses symlinks, so Cronos never writes through
/// a link an agent placed in the worktree.
pub fn ensure_real_directory(path: &Path) -> io::Result<PathBuf> {
    match std::fs::create_dir(path) {
        Ok(()) => {}
        // Parallel agents create the same directories; whatever exists is checked below.
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} must be a real directory", path.display()),
        ));
    }
    Ok(path.to_path_buf())
}

/// Replaces `path` with a new empty file. `create_new` never follows a symlink, so a link swapped in
/// after the removal makes this fail instead of truncating the link target.
fn create_fresh_file(path: &Path) -> io::Result<std::fs::File> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
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
    /// GitHub CLI; optional, needed only for pull-request publishing.
    gh: PathBuf,
}

impl Default for ToolCommands {
    fn default() -> Self {
        Self {
            pi: "pi".into(),
            herdr: "herdr".into(),
            openspec: "openspec".into(),
            git: "git".into(),
            gh: "gh".into(),
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
            gh: "gh".into(),
        }
    }

    pub fn with_gh(mut self, gh: PathBuf) -> Self {
        self.gh = gh;
        self
    }

    /// Runs Pi once, read-only, in the repository with only the planning skill and returns its
    /// final answer. Pi's own discovery is off and project-local Pi files are ignored; the whole
    /// process group is stopped when the time limit is reached.
    pub fn run_planner(
        &self,
        repository: &Path,
        skill: &Path,
        prompt: &str,
        timeout: Duration,
    ) -> Result<String, IntegrationError> {
        let args = [
            OsString::from("--tools"),
            "read,grep,find,ls".into(),
            "--skill".into(),
            skill.into(),
        ];
        self.run_one_shot_pi(repository, &args, prompt, timeout, "planner")
    }

    /// Runs Pi once, read-only, in a task worktree to plan the agents of a workflow step.
    pub fn run_step_planner(
        &self,
        worktree: &Path,
        model: Option<&str>,
        prompt: &str,
        timeout: Duration,
    ) -> Result<String, IntegrationError> {
        let mut args = vec![OsString::from("--tools"), "read,grep,find,ls".into()];
        if let Some(model) = model {
            args.extend(["--model".into(), model.into()]);
        }
        self.run_one_shot_pi(worktree, &args, prompt, timeout, "step planner")
    }

    /// Runs Pi once with no tools at all, on `model` when given, for a quick decision such as
    /// which task to start next.
    pub fn run_prioritizer(
        &self,
        directory: &Path,
        model: Option<&str>,
        prompt: &str,
        timeout: Duration,
    ) -> Result<String, IntegrationError> {
        let mut args = vec![OsString::from("--no-tools")];
        if let Some(model) = model {
            args.extend(["--model".into(), model.into()]);
        }
        self.run_one_shot_pi(directory, &args, prompt, timeout, "prioritizer")
    }

    fn run_one_shot_pi(
        &self,
        directory: &Path,
        args: &[OsString],
        prompt: &str,
        timeout: Duration,
        role: &str,
    ) -> Result<String, IntegrationError> {
        let io_error = |source| IntegrationError::Io { tool: "pi", source };
        let mut stdout = tempfile::tempfile().map_err(io_error)?;
        let mut stderr = tempfile::tempfile().map_err(io_error)?;
        let mut command = Command::new(&self.pi);
        command
            .args([
                "-p",
                "--mode",
                "text",
                "--no-session",
                "--no-skills",
                "--no-extensions",
                "--no-context-files",
                "--no-mcp",
                "--no-prompt-templates",
                "--no-approve",
            ])
            .args(args)
            .arg("--")
            .arg(prompt)
            .current_dir(directory)
            .stdin(Stdio::null())
            .stdout(stdout.try_clone().map_err(io_error)?)
            .stderr(stderr.try_clone().map_err(io_error)?);
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
        let mut child = command.spawn().map_err(io_error)?;
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().map_err(io_error)? {
                break status;
            }
            if started.elapsed() >= timeout {
                #[cfg(unix)]
                let _ = Command::new("kill")
                    .args(["-KILL", &format!("-{}", child.id())])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
                let _ = child.kill();
                let _ = child.wait();
                return Err(IntegrationError::Failed {
                    tool: "pi",
                    status: "timeout".to_owned(),
                    stderr: format!(
                        "the {role} exceeded its {}-minute time limit",
                        timeout.as_secs().div_ceil(60)
                    ),
                });
            }
            thread::sleep(Duration::from_millis(50));
        };
        let read = |file: &mut std::fs::File| -> Result<String, IntegrationError> {
            let mut text = String::new();
            file.seek(SeekFrom::Start(0)).map_err(io_error)?;
            file.read_to_string(&mut text).map_err(io_error)?;
            Ok(text)
        };
        if !status.success() {
            let stderr = read(&mut stderr)?;
            let lines = stderr.lines().collect::<Vec<_>>();
            return Err(IntegrationError::Failed {
                tool: "pi",
                status: status.to_string(),
                stderr: lines[lines.len().saturating_sub(20)..].join("\n"),
            });
        }
        read(&mut stdout)
    }

    fn run_gh(&self, dir: &Path, args: &[&str]) -> Result<Output, IntegrationError> {
        let args = args.iter().map(OsStr::new).collect::<Vec<_>>();
        run_program(&self.gh, "gh", Some(dir), &args)
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
                // Read-only file access plus the Cronos workflow tools; no shell, edit, or write.
                OsStr::new("--tools"),
                OsStr::new(ORCHESTRATOR_TOOLS),
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

    /// Types `command` into a pane's shell and runs it.
    pub fn herdr_run_in_pane(
        &self,
        pane_id: &str,
        command: &str,
    ) -> Result<Output, IntegrationError> {
        self.run(
            RequiredTool::Herdr,
            &[
                OsStr::new("pane"),
                OsStr::new("run"),
                OsStr::new(pane_id),
                OsStr::new(command),
            ],
        )
    }

    pub fn herdr_prompt_agent(
        &self,
        name: &str,
        prompt: &str,
        timeout: Duration,
    ) -> Result<Output, IntegrationError> {
        if !valid_agent_name(name) {
            return Err(IntegrationError::InvalidAgentName);
        }
        let timeout_ms = timeout.as_millis().to_string();
        self.run(
            RequiredTool::Herdr,
            &[
                OsStr::new("agent"),
                OsStr::new("prompt"),
                OsStr::new(name),
                OsStr::new(prompt),
                OsStr::new("--wait"),
                OsStr::new("--timeout"),
                OsStr::new(&timeout_ms),
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

    pub fn herdr_close_pane(&self, pane_id: &str) -> Result<Output, IntegrationError> {
        if pane_id.is_empty() {
            return Err(IntegrationError::InvalidHerdrResponse);
        }
        self.run(
            RequiredTool::Herdr,
            &[OsStr::new("pane"), OsStr::new("close"), OsStr::new(pane_id)],
        )
    }

    /// Pane that hosts a named agent, used to clean up agents left by an earlier process.
    pub fn herdr_agent_pane(&self, name: &str) -> Result<String, IntegrationError> {
        if !valid_agent_name(name) {
            return Err(IntegrationError::InvalidAgentName);
        }
        let output = self.run(
            RequiredTool::Herdr,
            &[OsStr::new("agent"), OsStr::new("get"), OsStr::new(name)],
        )?;
        let response: serde_json::Value = serde_json::from_slice(&output.stdout)
            .map_err(|_| IntegrationError::InvalidHerdrResponse)?;
        find_string_field(&response, "pane_id")
            .filter(|pane_id| !pane_id.is_empty())
            .map(str::to_owned)
            .ok_or(IntegrationError::InvalidHerdrResponse)
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

    /// OpenSpec resolves its root from the working directory, so run it inside the worktree.
    pub fn run_openspec_in(&self, dir: &Path, args: &[&str]) -> Result<Output, IntegrationError> {
        let args = args.iter().map(OsStr::new).collect::<Vec<_>>();
        self.run_in(RequiredTool::OpenSpec, Some(dir), &args)
    }

    pub fn run_git(&self, args: &[&OsStr]) -> Result<Output, IntegrationError> {
        self.run(RequiredTool::Git, args)
    }

    fn run(&self, tool: RequiredTool, args: &[&OsStr]) -> Result<Output, IntegrationError> {
        self.run_in(tool, None, args)
    }

    fn run_in(
        &self,
        tool: RequiredTool,
        dir: Option<&Path>,
        args: &[&OsStr],
    ) -> Result<Output, IntegrationError> {
        let executable = match tool {
            RequiredTool::Pi => &self.pi,
            RequiredTool::Herdr => &self.herdr,
            RequiredTool::OpenSpec => &self.openspec,
            RequiredTool::Git => &self.git,
        };
        run_program(executable, tool.executable(), dir, args)
    }
}

fn run_program(
    executable: &Path,
    tool: &'static str,
    dir: Option<&Path>,
    args: &[&OsStr],
) -> Result<Output, IntegrationError> {
    let mut command = Command::new(executable);
    command.args(args);
    // Cronos runs tools without a terminal: fail on missing credentials instead of prompting.
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GH_PROMPT_DISABLED", "1");
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    let output = command
        .output()
        .map_err(|source| IntegrationError::Io { tool, source })?;
    if !output.status.success() {
        return Err(IntegrationError::Failed {
            tool,
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(output)
}

fn last_line(output: &[u8]) -> Option<String> {
    String::from_utf8_lossy(output)
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .map(str::to_owned)
}

const ORCHESTRATOR_TOOLS: &str = "read,cronos_task_state,cronos_agent_profiles,cronos_run_step";
const WORKFLOW_EXTENSION: &str = include_str!("../extensions/cronos-workflow.ts");

/// Writes the embedded workflow extension to the data directory so the binary does not
/// depend on its source checkout. Rewrites the file only when its content differs.
pub fn install_workflow_extension(data_dir: &Path) -> io::Result<PathBuf> {
    let directory = ensure_real_directory(&data_dir.join("extensions"))?;
    install_embedded_file(&directory, "cronos-workflow.ts", WORKFLOW_EXTENSION)
}

/// Atomically writes `content` to `directory/file_name` unless the file already holds it. Refuses
/// a symlinked target so a planted link can never redirect the write.
pub fn install_embedded_file(
    directory: &Path,
    file_name: &str,
    content: &str,
) -> io::Result<PathBuf> {
    let path = directory.join(file_name);
    if std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} must not be a symlink", path.display()),
        ));
    }
    if std::fs::read_to_string(&path).is_ok_and(|current| current == content) {
        return Ok(path);
    }
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    io::Write::write_all(&mut temporary, content.as_bytes())?;
    temporary.persist(&path).map_err(|error| error.error)?;
    Ok(path)
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

fn find_string_field<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    match value {
        serde_json::Value::Object(object) => object
            .get(key)
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                object
                    .values()
                    .find_map(|value| find_string_field(value, key))
            }),
        serde_json::Value::Array(items) => {
            items.iter().find_map(|value| find_string_field(value, key))
        }
        _ => None,
    }
}

fn valid_agent_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('a'..='z'))
        && name.len() <= 32
        && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '_' | '-'))
}

pub(crate) fn quote_shell_word(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::{
        GitAdapter, GitError, IntegrationError, PullRequest, RequiredTool, ToolCommands,
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
            .herdr_prompt_agent(
                "reviewer-1",
                "review; do not execute shell syntax",
                std::time::Duration::from_secs(30 * 60),
            )
            .unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.contains("<review; do not execute shell syntax>"));
        assert!(stdout.contains("<1800000>"));
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

    fn repository_with_task_commit(temp: &Path) -> (PathBuf, PathBuf, PathBuf, &'static str) {
        let repository = temp.join("repo");
        let remote = temp.join("remote.git");
        let worktree = temp.join("worktrees/task/attempt-1");
        init_git_fixture(&repository);
        fixture_git(temp, &["init", "--bare", remote.to_str().unwrap()]);
        fixture_git(
            &repository,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        fs::create_dir_all(worktree.parent().unwrap()).unwrap();
        let branch = "cronos-ai/task/attempt-1";
        GitAdapter::default()
            .add_worktree(&repository, &worktree, branch)
            .unwrap();
        fs::write(worktree.join("generated.txt"), "task output\n").unwrap();
        fixture_git(&worktree, &["add", "generated.txt"]);
        fixture_git(&worktree, &["commit", "-m", "task output"]);
        (repository, remote, worktree, branch)
    }

    #[test]
    fn merged_branch_is_detected_and_push_publishes_the_base_branch() {
        let temp = tempfile::tempdir().unwrap();
        let (repository, remote, worktree, branch) = repository_with_task_commit(temp.path());
        let git = GitAdapter::default();

        assert!(!git.is_merged(&repository, branch).unwrap());
        git.merge_worktree(&repository, &worktree, branch).unwrap();
        assert!(git.is_merged(&repository, branch).unwrap());

        let base = git.current_branch(&repository).unwrap();
        git.push(&repository, "origin", &base).unwrap();
        assert_eq!(
            fixture_git(&remote, &["rev-parse", &base]),
            fixture_git(&repository, &["rev-parse", "HEAD"])
        );
    }

    #[test]
    fn review_diff_shows_task_changes_and_caps_its_length() {
        let temp = tempfile::tempdir().unwrap();
        let (repository, _, worktree, _) = repository_with_task_commit(temp.path());
        let git = GitAdapter::default();

        let diff = git.review_diff(&repository, &worktree, 1_000).unwrap();
        assert!(
            diff.starts_with("diff --git a/generated.txt b/generated.txt"),
            "{diff}"
        );
        assert!(diff.lines().any(|line| line == "+task output"));
        assert!(!diff.contains('\u{1b}'), "no color codes");

        let capped = git.review_diff(&repository, &worktree, 2).unwrap();
        assert_eq!(capped.lines().count(), 3);
        assert!(
            capped
                .lines()
                .last()
                .unwrap()
                .contains("more diff lines not shown")
        );
    }

    #[test]
    fn releases_are_found_listed_committed_tagged_and_pushed() {
        let temp = tempfile::tempdir().unwrap();
        let (repository, remote, worktree, branch) = repository_with_task_commit(temp.path());
        let git = GitAdapter::default();
        assert_eq!(git.latest_release(&repository).unwrap(), None);
        fixture_git(&repository, &["tag", "v0.1.0"]);
        fixture_git(&repository, &["tag", "v0.0.9"]);
        fixture_git(&repository, &["tag", "nightly"]);
        assert_eq!(
            git.latest_release(&repository).unwrap(),
            Some(crate::release::Version::new(0, 1, 0))
        );

        git.merge_worktree(&repository, &worktree, branch).unwrap();
        let commits = git.commits(&repository, "v0.1.0..HEAD").unwrap();
        assert_eq!(commits.len(), 1, "merge commits are left out");
        assert_eq!(commits[0].subject, "task output");
        assert_eq!(git.release_at_head(&repository).unwrap(), None);

        git.commit_release(
            &repository,
            "CHANGELOG.md",
            "# Changelog\n",
            &crate::release::Version::new(0, 1, 1),
            "Release v0.1.1",
        )
        .unwrap();
        assert_eq!(
            git.release_at_head(&repository).unwrap(),
            Some(crate::release::Version::new(0, 1, 1))
        );
        assert_eq!(
            fixture_git(&repository, &["log", "-1", "--format=%s"]).trim(),
            "chore(release): v0.1.1"
        );
        assert!(git.is_clean(&repository).unwrap());

        git.push_tag(&repository, "origin", "v0.1.1").unwrap();
        assert_eq!(
            fixture_git(&remote, &["rev-parse", "v0.1.1^{commit}"]),
            fixture_git(&repository, &["rev-parse", "HEAD"])
        );
    }

    use std::time::Duration;

    fn fake_pi(directory: &Path, body: &str) -> PathBuf {
        let path = directory.join("pi");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&path, permissions).unwrap();
        path
    }

    #[test]
    fn the_planner_runs_read_only_in_the_repository_and_returns_its_answer() {
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repo");
        fs::create_dir_all(&repository).unwrap();
        let pi = fake_pi(
            temp.path(),
            r#"printf '%s\n' "$@" > "$(dirname "$0")/pi-args"; pwd > "$(dirname "$0")/pi-cwd"; printf 'plan answer\n'"#,
        );
        let tools = ToolCommands::with_paths(pi.clone(), pi.clone(), pi.clone(), "git".into());

        let answer = tools
            .run_planner(
                &repository,
                Path::new("/skills/planning/SKILL.md"),
                "Plan it",
                Duration::from_secs(10),
            )
            .unwrap();

        assert_eq!(answer, "plan answer\n");
        let args = fs::read_to_string(temp.path().join("pi-args")).unwrap();
        let args = args.lines().collect::<Vec<_>>();
        for flag in [
            "-p",
            "--no-session",
            "--no-skills",
            "--no-extensions",
            "--no-context-files",
            "--no-mcp",
            "--no-approve",
        ] {
            assert!(args.contains(&flag), "{flag} missing from {args:?}");
        }
        let tools_at = args.iter().position(|arg| *arg == "--tools").unwrap();
        assert_eq!(args[tools_at + 1], "read,grep,find,ls");
        let skill_at = args.iter().position(|arg| *arg == "--skill").unwrap();
        assert_eq!(args[skill_at + 1], "/skills/planning/SKILL.md");
        assert_eq!(&args[args.len() - 2..], ["--", "Plan it"]);
        assert_eq!(
            fs::canonicalize(
                fs::read_to_string(temp.path().join("pi-cwd"))
                    .unwrap()
                    .trim()
            )
            .unwrap(),
            fs::canonicalize(&repository).unwrap()
        );
    }

    #[test]
    fn the_prioritizer_runs_without_tools_on_the_chosen_model() {
        let temp = tempfile::tempdir().unwrap();
        let pi = fake_pi(
            temp.path(),
            r#"printf '%s\n' "$@" > "$(dirname "$0")/pi-args"; printf '{"order": ["T1"]}\n'"#,
        );
        let tools = ToolCommands::with_paths(pi.clone(), pi.clone(), pi, "git".into());

        let answer = tools
            .run_prioritizer(
                temp.path(),
                Some("provider/jev"),
                "Rank",
                Duration::from_secs(10),
            )
            .unwrap();

        assert_eq!(answer.trim(), "{\"order\": [\"T1\"]}");
        let args = fs::read_to_string(temp.path().join("pi-args")).unwrap();
        let args = args.lines().collect::<Vec<_>>();
        assert!(args.contains(&"--no-tools"), "{args:?}");
        assert!(!args.contains(&"--skill"));
        let model = args.iter().position(|arg| *arg == "--model").unwrap();
        assert_eq!(args[model + 1], "provider/jev");
        assert_eq!(&args[args.len() - 2..], ["--", "Rank"]);
    }

    #[test]
    fn a_failing_or_slow_planner_is_reported() {
        let temp = tempfile::tempdir().unwrap();
        let failing = fake_pi(temp.path(), "echo 'no credentials' >&2; exit 3");
        let tools =
            ToolCommands::with_paths(failing.clone(), failing.clone(), failing, "git".into());
        let error = tools
            .run_planner(temp.path(), Path::new("/s"), "p", Duration::from_secs(10))
            .unwrap_err();
        assert!(error.to_string().contains("no credentials"), "{error}");

        let slow_dir = temp.path().join("slow");
        fs::create_dir_all(&slow_dir).unwrap();
        let slow = fake_pi(&slow_dir, "sleep 30");
        let tools = ToolCommands::with_paths(slow.clone(), slow.clone(), slow, "git".into());
        let started = std::time::Instant::now();
        let error = tools
            .run_planner(
                temp.path(),
                Path::new("/s"),
                "p",
                Duration::from_millis(300),
            )
            .unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(error.to_string().contains("time limit"), "{error}");
    }

    #[test]
    fn push_failure_reports_the_remote_error() {
        let temp = tempfile::tempdir().unwrap();
        let (repository, _, _, branch) = repository_with_task_commit(temp.path());

        let error = GitAdapter::default()
            .push(&repository, "missing-remote", branch)
            .unwrap_err();

        assert!(
            matches!(&error, GitError::PushFailed(detail) if detail.contains("missing-remote")),
            "{error}"
        );
    }

    #[test]
    fn pull_request_pushes_the_task_branch_and_reuses_an_open_request() {
        let temp = tempfile::tempdir().unwrap();
        let (repository, remote, worktree, branch) = repository_with_task_commit(temp.path());
        let gh = temp.path().join("gh");
        fs::write(
            &gh,
            r#"#!/bin/sh
printf '%s\n' "$*" >> "$(dirname "$0")/gh-calls"
if [ "$1 $2" = "pr view" ]; then
  [ -f "$(dirname "$0")/pr-open" ] || exit 1
  printf 'https://example.invalid/pull/7\n'
elif [ "$1 $2" = "pr create" ]; then
  touch "$(dirname "$0")/pr-open"
  printf 'Creating pull request\nhttps://example.invalid/pull/7\n'
fi
"#,
        )
        .unwrap();
        let mut permissions = fs::metadata(&gh).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&gh, permissions).unwrap();
        let git = GitAdapter::with_tools(ToolCommands::default().with_gh(gh));
        let base = git.current_branch(&repository).unwrap();

        for _ in 0..2 {
            let url = git
                .open_pull_request(
                    &repository,
                    &worktree,
                    &PullRequest {
                        remote: "origin",
                        base: &base,
                        head: branch,
                        title: "Add output",
                        body: "Body",
                    },
                )
                .unwrap();
            assert_eq!(url, "https://example.invalid/pull/7");
        }

        assert_eq!(
            fixture_git(&remote, &["rev-parse", branch]),
            fixture_git(&worktree, &["rev-parse", "HEAD"])
        );
        assert!(!git.is_merged(&repository, branch).unwrap());
        let calls = fs::read_to_string(temp.path().join("gh-calls")).unwrap();
        assert_eq!(calls.matches("pr create").count(), 1, "{calls}");
        assert!(calls.contains(&format!(
            "pr create --base {base} --head {branch} --title Add output --body Body"
        )));
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
                OsStr::new("--tools"),
                OsStr::new("read,cronos_task_state,cronos_agent_profiles,cronos_run_step"),
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

    #[test]
    fn embedded_workflow_extension_is_installed_into_the_data_directory() {
        let temp = tempfile::tempdir().unwrap();
        let path = super::install_workflow_extension(temp.path()).unwrap();
        assert_eq!(path, temp.path().join("extensions/cronos-workflow.ts"));
        let installed = fs::read_to_string(&path).unwrap();
        assert!(installed.contains("cronos_run_step"));
        fs::write(&path, "stale").unwrap();
        super::install_workflow_extension(temp.path()).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), installed);
    }

    #[test]
    fn check_command_reports_exit_status_and_output_tail_without_a_shell() {
        let temp = tempfile::tempdir().unwrap();
        let script = temp.path().join("check.sh");
        fs::write(&script, "#!/bin/sh\necho \"checked $1\"\nexit \"$2\"\n").unwrap();
        let mut permissions = fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&script, permissions).unwrap();
        let run = |code: &str| {
            super::run_check_command(
                temp.path(),
                &["./check.sh".to_owned(), "a;b".to_owned(), code.to_owned()],
                std::time::Duration::from_secs(10),
            )
            .unwrap()
        };
        let passed = run("0");
        assert!(passed.success);
        assert_eq!(passed.output_tail, "checked a;b");
        assert!(!run("3").success);
        assert!(temp.path().join(".cronos/check-output.log").is_file());

        let slow = super::run_check_command(
            temp.path(),
            &["sleep".to_owned(), "5".to_owned()],
            std::time::Duration::from_millis(200),
        )
        .unwrap();
        assert!(!slow.success);
        assert!(slow.output_tail.contains("exceeded"));
    }

    #[test]
    fn check_command_never_writes_through_a_planted_symlink() {
        let temp = tempfile::tempdir().unwrap();
        let victim = temp.path().join("victim.txt");
        fs::write(&victim, "keep me\n").unwrap();
        let worktree = temp.path().join("worktree");
        fs::create_dir_all(worktree.join(".cronos")).unwrap();
        std::os::unix::fs::symlink(&victim, worktree.join(".cronos/check-output.log")).unwrap();
        let outcome = super::run_check_command(
            &worktree,
            &["echo".to_owned(), "checked".to_owned()],
            std::time::Duration::from_secs(10),
        )
        .unwrap();
        assert!(outcome.success);
        assert_eq!(fs::read_to_string(&victim).unwrap(), "keep me\n");
        assert!(
            !fs::symlink_metadata(worktree.join(".cronos/check-output.log"))
                .unwrap()
                .file_type()
                .is_symlink()
        );

        // A symlinked .cronos directory is refused outright.
        let linked = temp.path().join("linked");
        fs::create_dir_all(&linked).unwrap();
        std::os::unix::fs::symlink(temp.path(), linked.join(".cronos")).unwrap();
        assert!(
            super::run_check_command(
                &linked,
                &["echo".to_owned()],
                std::time::Duration::from_secs(10)
            )
            .is_err()
        );
    }

    #[test]
    fn check_command_timeout_stops_processes_it_started() {
        let temp = tempfile::tempdir().unwrap();
        let marker = temp.path().join("survivor");
        let script = temp.path().join("check.sh");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\n(sleep 1; touch '{}') &\nsleep 5\n",
                marker.display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&script, permissions).unwrap();
        let outcome = super::run_check_command(
            temp.path(),
            &["./check.sh".to_owned()],
            std::time::Duration::from_millis(200),
        )
        .unwrap();
        assert!(!outcome.success);
        std::thread::sleep(std::time::Duration::from_millis(1500));
        assert!(
            !marker.exists(),
            "a child of the check survived its timeout"
        );
    }
}
