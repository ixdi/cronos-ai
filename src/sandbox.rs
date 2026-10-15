//! Write sandbox for workflow workers. A worker may write anywhere its tools need (build output,
//! caches, temporary files) except Cronos-AI's own data and the project's base checkout; it keeps
//! its own worktree and the repository's Git objects so it can commit, but not the hooks and
//! configuration Git runs and reads when Cronos merges.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use crate::integrations::{GitAdapter, quote_shell_word};

/// The paths a sandboxed worker of one project is kept away from, all canonical.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SandboxPolicy {
    pub data_dir: PathBuf,
    pub repository: PathBuf,
    /// The repository's common Git directory, shared by every worktree.
    pub git_dir: PathBuf,
}

impl SandboxPolicy {
    /// The policy for a project, with every path resolved the way the kernel sees it (the
    /// sandbox compares real paths, so symlinks such as macOS's /var must be resolved).
    pub fn for_project(
        data_dir: &Path,
        repository: &Path,
        git: &GitAdapter,
    ) -> Result<Self, String> {
        let canonical = |path: &Path| {
            std::fs::canonicalize(path)
                .map_err(|error| format!("could not resolve {}: {error}", path.display()))
        };
        let repository = canonical(
            &git.repository_root(repository)
                .map_err(|error| error.to_string())?,
        )?;
        let git_dir = canonical(
            &git.common_dir(&repository)
                .map_err(|error| error.to_string())?,
        )?;
        Ok(Self {
            data_dir: canonical(data_dir)?,
            repository,
            git_dir,
        })
    }
}

/// The text typed into a worker's pane before Pi starts: it replaces the pane's shell with an
/// interactive shell inside the sandbox, so Pi and everything it runs inherit the restrictions.
pub fn sandboxed_shell_command(
    policy: &SandboxPolicy,
    worktree: &Path,
    macos_profile_path: &Path,
    shell: &Path,
) -> Result<String, String> {
    let words: Vec<OsString> = if cfg!(target_os = "macos") {
        vec![
            "/usr/bin/sandbox-exec".into(),
            "-f".into(),
            macos_profile_path.into(),
            shell.into(),
            "-i".into(),
        ]
    } else if cfg!(target_os = "linux") {
        let mut words = vec![OsString::from("bwrap")];
        words.extend(linux_shell_args(policy, worktree, shell));
        words
    } else {
        return Err("worker sandboxing is supported on macOS and Linux only".to_owned());
    };
    let quoted = words
        .iter()
        .map(|word| quote_shell_word(&word.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(" ");
    Ok(format!("exec {quoted}"))
}

/// Checks that this machine can sandbox workers, so a sandboxed project never runs unprotected.
pub fn sandbox_available() -> Result<(), String> {
    if cfg!(target_os = "macos") {
        return Path::new("/usr/bin/sandbox-exec")
            .is_file()
            .then_some(())
            .ok_or_else(|| "worker sandboxing needs /usr/bin/sandbox-exec".to_owned());
    }
    if cfg!(target_os = "linux") {
        return std::process::Command::new("bwrap")
            .arg("--version")
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|_| ())
            .ok_or_else(|| {
                "worker sandboxing needs bubblewrap (bwrap); install it or turn the sandbox off"
                    .to_owned()
            });
    }
    Err("worker sandboxing is supported on macOS and Linux only".to_owned())
}

/// A quoted SBPL string literal.
fn sbpl_string(path: &Path) -> String {
    let text = path.to_string_lossy();
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A `sandbox-exec` profile for a worker in `worktree`. Later rules take precedence, so each
/// exception follows the rule it narrows.
pub fn macos_profile(policy: &SandboxPolicy, worktree: &Path) -> String {
    let rule = |action: &str, filter: &str, path: &Path| {
        format!("({action} file-write* ({filter} {}))\n", sbpl_string(path))
    };
    [
        "(version 1)\n(allow default)\n".to_owned(),
        rule("deny", "subpath", &policy.data_dir),
        rule("deny", "subpath", &policy.repository),
        rule("allow", "subpath", worktree),
        rule("allow", "subpath", &policy.git_dir),
        rule("deny", "subpath", &policy.git_dir.join("hooks")),
        rule("deny", "literal", &policy.git_dir.join("config")),
    ]
    .concat()
}

/// `bwrap` arguments that run `shell` interactively under the same rules; later binds override
/// earlier ones.
pub fn linux_shell_args(policy: &SandboxPolicy, worktree: &Path, shell: &Path) -> Vec<OsString> {
    let mut args = vec!["--dev-bind".into(), "/".into(), "/".into()];
    let mut bind = |flag: &str, path: &Path| {
        args.push(flag.into());
        args.push(path.into());
        args.push(path.into());
    };
    bind("--ro-bind", &policy.data_dir);
    bind("--ro-bind", &policy.repository);
    bind("--bind", worktree);
    bind("--bind", &policy.git_dir);
    bind("--ro-bind-try", &policy.git_dir.join("hooks"));
    bind("--ro-bind-try", &policy.git_dir.join("config"));
    args.extend(["--".into(), shell.into(), "-i".into()]);
    args
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{SandboxPolicy, linux_shell_args, macos_profile, sbpl_string};

    fn policy() -> SandboxPolicy {
        SandboxPolicy {
            data_dir: "/data".into(),
            repository: "/repo".into(),
            git_dir: "/repo/.git".into(),
        }
    }

    #[test]
    fn the_macos_profile_denies_cronos_data_and_the_base_checkout_but_not_the_worktree() {
        let profile = macos_profile(&policy(), Path::new("/data/worktrees/p/t/attempt-1"));

        assert!(profile.starts_with("(version 1)\n(allow default)\n"));
        let position = |rule: &str| {
            profile
                .find(rule)
                .unwrap_or_else(|| panic!("missing {rule} in\n{profile}"))
        };
        // Later rules win, so each exception follows the rule it narrows.
        let data = position("(deny file-write* (subpath \"/data\"))");
        let repository = position("(deny file-write* (subpath \"/repo\"))");
        let worktree = position("(allow file-write* (subpath \"/data/worktrees/p/t/attempt-1\"))");
        let git = position("(allow file-write* (subpath \"/repo/.git\"))");
        let hooks = position("(deny file-write* (subpath \"/repo/.git/hooks\"))");
        let config = position("(deny file-write* (literal \"/repo/.git/config\"))");
        assert!(data < worktree && repository < git && git < hooks && git < config);
    }

    #[test]
    fn a_project_policy_uses_canonical_paths_and_the_common_git_directory() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let repository = root.join("repo");
        std::fs::create_dir_all(&repository).unwrap();
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&repository)
                .arg("init")
                .output()
                .unwrap()
                .status
                .success()
        );
        std::fs::create_dir_all(root.join("data")).unwrap();

        let policy = SandboxPolicy::for_project(
            &temp.path().join("data"),
            &repository.join("."),
            &crate::integrations::GitAdapter::default(),
        )
        .unwrap();

        assert_eq!(policy.data_dir, root.join("data"));
        assert_eq!(policy.repository, repository);
        assert_eq!(policy.git_dir, repository.join(".git"));
    }

    #[test]
    fn the_pane_command_replaces_the_shell_with_a_sandboxed_one() {
        let command = super::sandboxed_shell_command(
            &policy(),
            Path::new("/data/worktrees/p/t/attempt-1"),
            Path::new("/data/sandbox/it's.sb"),
            Path::new("/bin/zsh"),
        )
        .unwrap();

        if cfg!(target_os = "macos") {
            assert_eq!(
                command,
                "exec '/usr/bin/sandbox-exec' '-f' '/data/sandbox/it'\\''s.sb' '/bin/zsh' '-i'"
            );
        } else {
            assert!(
                command.starts_with("exec 'bwrap' '--dev-bind' '/' '/'"),
                "{command}"
            );
            assert!(command.ends_with("'--' '/bin/zsh' '-i'"), "{command}");
        }
    }

    #[test]
    fn sbpl_strings_escape_quotes_and_backslashes() {
        assert_eq!(
            sbpl_string(Path::new("/a \"b\"\\c")),
            "\"/a \\\"b\\\"\\\\c\""
        );
    }

    #[test]
    fn the_linux_command_binds_the_same_rules_in_order() {
        let args = linux_shell_args(
            &policy(),
            Path::new("/data/worktrees/p/t/attempt-1"),
            Path::new("/bin/zsh"),
        );
        let args = args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(&args[..3], ["--dev-bind", "/", "/"]);
        let pair = |flag: &str, path: &str| {
            args.windows(3)
                .position(|window| window[0] == flag && window[1] == path && window[2] == path)
                .unwrap_or_else(|| panic!("missing {flag} {path} in {args:?}"))
        };
        assert!(pair("--ro-bind", "/data") < pair("--bind", "/data/worktrees/p/t/attempt-1"));
        assert!(pair("--ro-bind", "/repo") < pair("--bind", "/repo/.git"));
        // Hooks and config may be missing in unusual repositories, so they bind only if present.
        assert!(pair("--bind", "/repo/.git") < pair("--ro-bind-try", "/repo/.git/hooks"));
        assert!(pair("--bind", "/repo/.git") < pair("--ro-bind-try", "/repo/.git/config"));
        assert_eq!(&args[args.len() - 3..], ["--", "/bin/zsh", "-i"]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sandboxed_processes_cannot_write_cronos_data_the_checkout_or_git_hooks() {
        use std::process::Command;

        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let data = root.join("data");
        let worktree = data.join("worktrees/p/t/attempt-1");
        let repository = root.join("repo");
        for directory in [
            &worktree,
            &repository.join(".git/hooks"),
            &repository.join(".git/objects"),
        ] {
            std::fs::create_dir_all(directory).unwrap();
        }
        std::fs::write(repository.join(".git/config"), "[core]\n").unwrap();
        let elsewhere = root.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let policy = SandboxPolicy {
            data_dir: data.clone(),
            repository: repository.clone(),
            git_dir: repository.join(".git"),
        };
        let profile = root.join("worker.sb");
        std::fs::write(&profile, macos_profile(&policy, &worktree)).unwrap();
        let writes = |path: &Path| {
            Command::new("/usr/bin/sandbox-exec")
                .arg("-f")
                .arg(&profile)
                .args(["/bin/sh", "-c", "echo x > \"$0\""])
                .arg(path)
                .status()
                .unwrap()
                .success()
        };

        assert!(
            writes(&worktree.join("code.rs")),
            "the worktree is writable"
        );
        assert!(
            writes(&repository.join(".git/objects/ab")),
            "commits can store objects"
        );
        assert!(
            writes(&elsewhere.join("cache")),
            "other locations stay writable"
        );
        assert!(
            !writes(&data.join("projects.json")),
            "Cronos data is protected"
        );
        assert!(
            !writes(&repository.join("README.md")),
            "the base checkout is protected"
        );
        assert!(
            !writes(&repository.join(".git/hooks/post-merge")),
            "hooks are protected"
        );
        assert!(
            !writes(&repository.join(".git/config")),
            "Git config is protected"
        );
    }
}
