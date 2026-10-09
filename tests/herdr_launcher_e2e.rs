#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

use tempfile::tempdir;

fn executable(path: &Path, content: &str) {
    fs::write(path, content).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(path, permissions).unwrap();
}

#[test]
fn launcher_creates_workspace_runs_cronos_and_attaches_herdr_session() {
    let temp = tempdir().unwrap();
    let bin = temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let calls = temp.path().join("herdr-args");
    executable(
        &bin.join("herdr"),
        "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$HERDR_CALLS\"\nif [ \"$3\" = \"workspace\" ] && [ \"$4\" = \"create\" ]; then printf '%s' '{\"result\":{\"root_pane\":{\"pane_id\":\"w7:p3\"}}}'; fi\n",
    );
    let search_path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let cwd = temp.path().join("project with spaces");
    fs::create_dir(&cwd).unwrap();
    let cwd = cwd.canonicalize().unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_cronos-ai"))
        .args(["internal", "launch"])
        .current_dir(&cwd)
        .env("PATH", search_path)
        .env("HERDR_CALLS", &calls)
        .env("CRONOS_AI_DATA_DIR", temp.path().join("data"))
        .env_remove("HERDR_ENV")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "launcher failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let args: Vec<_> = fs::read_to_string(calls)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert!(
        args.windows(4)
            .any(|args| args == ["--session", "cronos-ai", "workspace", "create"])
    );
    assert!(
        args.windows(4)
            .any(|args| args == ["--label", "Cronos-AI", "--focus", "--session"])
    );
    assert!(args.windows(3).any(|args| args == ["pane", "run", "w7:p3"]));
    assert!(args.contains(&"--cwd".to_owned()));
    assert!(args.contains(&cwd.to_string_lossy().into_owned()));
    assert!(args.ends_with(&["--session".to_owned(), "cronos-ai".to_owned()]));
    assert!(
        args.iter()
            .any(|arg| arg.contains("target/debug/cronos-ai"))
    );
}
