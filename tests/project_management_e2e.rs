#![cfg(unix)]

//! Project and task management through the `cronos-ai` binary, as a user drives it.

use std::{path::Path, process::Command};

use serde_json::Value;

fn cronos(data_dir: &Path, args: &[&str]) -> Result<Value, String> {
    let output = Command::new(env!("CARGO_BIN_EXE_cronos-ai"))
        .arg("--data-dir")
        .arg(data_dir)
        .args(args)
        .output()
        .unwrap();
    if output.status.success() {
        serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    }
}

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

fn id(value: &Value) -> String {
    value["id"].as_str().unwrap().to_owned()
}

#[test]
fn projects_and_tasks_are_created_ordered_and_archived_from_the_cli() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let repository = temp.path().join("repo");
    std::fs::create_dir_all(&repository).unwrap();
    git(&repository, &["init"]);
    let repository_arg = repository.to_string_lossy().into_owned();

    let project = cronos(
        &data,
        &[
            "project",
            "create",
            "Factory",
            "--description",
            "End-to-end",
            "--repository",
            &repository_arg,
        ],
    )
    .unwrap();
    let project_id = id(&project);
    assert_eq!(project["name"], "Factory");
    assert_eq!(
        project["repository"].as_str().unwrap(),
        std::fs::canonicalize(&repository)
            .unwrap()
            .to_string_lossy()
    );
    assert!(project["check_command"].is_null());

    // The check command keeps its arguments separate; nothing goes through a shell.
    let configured = cronos(
        &data,
        &[
            "project",
            "set-check-command",
            &project_id,
            "--",
            "./scripts/check.sh",
            "--all",
            "a b",
        ],
    )
    .unwrap();
    assert_eq!(
        configured["check_command"],
        serde_json::json!(["./scripts/check.sh", "--all", "a b"])
    );

    let base = cronos(
        &data,
        &["task", "create", &project_id, "Base", "--priority", "low"],
    )
    .unwrap();
    let urgent = cronos(
        &data,
        &[
            "task",
            "create",
            &project_id,
            "Urgent",
            "--priority",
            "high",
        ],
    )
    .unwrap();
    let dependent = cronos(
        &data,
        &[
            "task",
            "create",
            &project_id,
            "Dependent",
            "--priority",
            "high",
            "--depends-on",
            &id(&base),
        ],
    )
    .unwrap();
    assert_eq!(dependent["status"], "pending");

    // Ready queue: priority first, and a task waits until its dependencies complete.
    let ready = cronos(&data, &["task", "ready", &project_id]).unwrap();
    let ready_ids = ready.as_array().unwrap().iter().map(id).collect::<Vec<_>>();
    assert_eq!(ready_ids, vec![id(&urgent), id(&base)]);
    assert_eq!(
        cronos(&data, &["task", "list", &project_id])
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        3
    );

    assert!(
        cronos(
            &data,
            &[
                "task",
                "create",
                &project_id,
                "Bad dependency",
                "--depends-on",
                &uuid_like()
            ],
        )
        .is_err()
    );

    let updated = cronos(
        &data,
        &["project", "update", &project_id, "--name", "Factory 2"],
    )
    .unwrap();
    assert_eq!(updated["name"], "Factory 2");
    let listed = cronos(&data, &["project", "list"]).unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1);

    let archived = cronos(&data, &["project", "archive", &project_id]).unwrap();
    assert_eq!(archived["status"], "archived");
    assert!(cronos(&data, &["task", "create", &project_id, "Late"]).is_err());
    assert!(cronos(&data, &["project", "select", &project_id]).is_err());
}

fn uuid_like() -> String {
    "00000000-0000-4000-8000-000000000000".to_owned()
}

#[test]
fn tasks_created_by_concurrent_processes_are_all_kept() {
    const PROCESSES: usize = 16;
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let project_id = id(&cronos(&data, &["project", "create", "Busy"]).unwrap());

    // Start every process before waiting for any, so their read-modify-save cycles overlap.
    let children = (0..PROCESSES)
        .map(|index| {
            Command::new(env!("CARGO_BIN_EXE_cronos-ai"))
                .arg("--data-dir")
                .arg(&data)
                .args(["task", "create", &project_id, &format!("Task {index}")])
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect::<Vec<_>>();
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }

    let listed = cronos(&data, &["task", "list", &project_id]).unwrap();
    assert_eq!(listed.as_array().unwrap().len(), PROCESSES);
    let stored = std::fs::read_dir(data.join("projects").join(&project_id).join("tasks"))
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|extension| extension == "json")
        })
        .count();
    assert_eq!(stored, PROCESSES, "every saved task file is listed");
}
