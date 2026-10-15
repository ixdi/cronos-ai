#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

use tempfile::tempdir;

fn executable(path: &Path, content: &str) {
    fs::write(path, content).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(path, permissions).unwrap();
}

fn git(cwd: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap()
}

/// Builds a repository that uses the tracked hooks directory, with a fake
/// check script that exits with `check_status`, and a bare remote to push to.
fn repository_with_hook(root: &Path, check_status: i32) -> std::path::PathBuf {
    let remote = root.join("remote.git");
    let repo = root.join("repo");
    assert!(
        git(root, &["init", "--bare", remote.to_str().unwrap()])
            .status
            .success()
    );
    assert!(
        git(root, &["init", repo.to_str().unwrap()])
            .status
            .success()
    );

    let hooks = repo.join(".githooks");
    fs::create_dir(&hooks).unwrap();
    let hook = Path::new(env!("CARGO_MANIFEST_DIR")).join(".githooks/pre-push");
    fs::copy(&hook, hooks.join("pre-push")).unwrap();

    let scripts = repo.join("scripts");
    fs::create_dir(&scripts).unwrap();
    executable(
        &scripts.join("check.sh"),
        &format!(
            "#!/bin/sh\necho check ran >> \"$(dirname \"$0\")/../check-ran\"\nexit {check_status}\n"
        ),
    );

    for args in [
        &["config", "user.email", "test@example.com"][..],
        &["config", "user.name", "Test"],
        &["config", "core.hooksPath", ".githooks"],
        &["remote", "add", "origin", remote.to_str().unwrap()],
        &["add", ".githooks", "scripts"],
        &["commit", "-m", "init"],
    ] {
        let output = git(&repo, args);
        assert!(output.status.success(), "{output:?}");
    }
    repo
}

#[test]
fn pre_push_hook_is_executable() {
    let hook = Path::new(env!("CARGO_MANIFEST_DIR")).join(".githooks/pre-push");
    let mode = fs::metadata(hook).unwrap().permissions().mode();
    assert_ne!(mode & 0o111, 0, "the pre-push hook must be executable");
}

#[test]
fn failing_checks_block_the_push() {
    let temp = tempdir().unwrap();
    let repo = repository_with_hook(temp.path(), 1);

    let output = git(&repo, &["push", "origin", "HEAD:main"]);

    assert!(!output.status.success(), "push should be rejected");
    assert!(repo.join("check-ran").exists());
    let remote_refs = git(&temp.path().join("remote.git"), &["show-ref"]);
    assert!(String::from_utf8_lossy(&remote_refs.stdout).is_empty());
}

#[test]
fn passing_checks_allow_the_push() {
    let temp = tempdir().unwrap();
    let repo = repository_with_hook(temp.path(), 0);

    let output = git(&repo, &["push", "origin", "HEAD:main"]);

    assert!(output.status.success(), "{output:?}");
    assert!(repo.join("check-ran").exists());
}
