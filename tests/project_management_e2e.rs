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

#[test]
fn check_time_limit_is_set_rejected_when_out_of_range_and_reset_from_the_cli() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let project_id = id(&cronos(&data, &["project", "create", "Timed"]).unwrap());

    let configured = cronos(&data, &["project", "set-check-timeout", &project_id, "90"]).unwrap();
    assert_eq!(configured["check_timeout_minutes"], 90);

    let error = cronos(&data, &["project", "set-check-timeout", &project_id, "481"]).unwrap_err();
    assert!(error.contains("between 1 and 480 minutes"), "{error}");

    let reset = cronos(&data, &["project", "set-check-timeout", &project_id]).unwrap();
    assert!(reset["check_timeout_minutes"].is_null());
}

#[test]
fn publish_mode_and_remote_are_set_and_validated_from_the_cli() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let project = cronos(&data, &["project", "create", "Published"]).unwrap();
    let project_id = id(&project);
    assert_eq!(project["publish"], "local");

    let configured = cronos(
        &data,
        &[
            "project",
            "set-publish",
            &project_id,
            "pull-request",
            "--remote",
            "upstream",
        ],
    )
    .unwrap();
    assert_eq!(configured["publish"], "pull_request");
    assert_eq!(configured["remote"], "upstream");

    let error = cronos(
        &data,
        &["project", "set-publish", &project_id, "push", "--remote=-c"],
    )
    .unwrap_err();
    assert!(error.contains("remote name"), "{error}");

    let reset = cronos(&data, &["project", "set-publish", &project_id, "push"]).unwrap();
    assert_eq!(reset["publish"], "push");
    assert!(reset["remote"].is_null());
}

#[test]
fn releases_are_switched_on_and_off_from_the_cli() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let project = cronos(&data, &["project", "create", "Released"]).unwrap();
    let project_id = id(&project);
    assert_eq!(project["releases"], false);

    let on = cronos(&data, &["project", "set-releases", &project_id, "on"]).unwrap();
    assert_eq!(on["releases"], true);
    let off = cronos(&data, &["project", "set-releases", &project_id, "off"]).unwrap();
    assert_eq!(off["releases"], false);
    assert!(cronos(&data, &["project", "set-releases", &project_id, "maybe"]).is_err());
}

#[test]
fn requirements_are_added_listed_and_removed_from_the_cli() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let project_id = id(&cronos(&data, &["project", "create", "Planned"]).unwrap());

    let export = cronos(
        &data,
        &[
            "requirement",
            "add",
            &project_id,
            "  Users can  export CSV ",
        ],
    )
    .unwrap();
    assert_eq!(export["text"], "Users can export CSV");
    assert_eq!(export["kind"], "functional");
    let speed = cronos(
        &data,
        &[
            "requirement",
            "add",
            &project_id,
            "--kind",
            "non-functional",
            "Exports finish within 2 seconds",
        ],
    )
    .unwrap();
    assert_eq!(speed["kind"], "non_functional");
    let duplicate = cronos(
        &data,
        &["requirement", "add", &project_id, "users can export csv"],
    )
    .unwrap_err();
    assert!(duplicate.contains("already listed"), "{duplicate}");

    let listed = cronos(&data, &["requirement", "list", &project_id]).unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 2);
    cronos(&data, &["requirement", "remove", &project_id, &id(&export)]).unwrap();
    let listed = cronos(&data, &["requirement", "list", &project_id]).unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["text"], "Exports finish within 2 seconds");
}

#[test]
fn tasks_are_planned_from_requirements_reviewed_and_applied_from_the_cli() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let repository = temp.path().join("repo");
    std::fs::create_dir_all(&repository).unwrap();
    git(&repository, &["init"]);
    let bin = temp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(
        bin.join("pi"),
        r#"#!/bin/sh
cat <<'PLAN'
Here is the plan.
```json
{"tasks": [
  {"key": "csv", "title": "Add CSV export", "priority": "high", "depends_on": [], "requirements": ["R1"]},
  {"key": "fast", "title": "Stream large exports", "priority": "medium", "depends_on": ["csv"], "requirements": ["R2"]}
]}
```
PLAN
"#,
    )
    .unwrap();
    std::fs::set_permissions(bin.join("pi"), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let run = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_cronos-ai"))
            .arg("--data-dir")
            .arg(&data)
            .args(args)
            .env("PATH", &path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };

    let project_id = id(&run(&[
        "project",
        "create",
        "Planned",
        "--repository",
        repository.to_str().unwrap(),
    ]));
    run(&["requirement", "add", &project_id, "Users can export CSV"]);
    run(&[
        "requirement",
        "add",
        &project_id,
        "--kind",
        "non-functional",
        "Exports stream in constant memory",
    ]);

    let plan = run(&["project", "plan", &project_id]);
    assert_eq!(plan["tasks"].as_array().unwrap().len(), 2);
    assert_eq!(
        run(&["task", "list", &project_id])
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(run(&["project", "plan-show", &project_id]), plan);

    let tasks = run(&["project", "plan-apply", &project_id]);
    assert_eq!(tasks.as_array().unwrap().len(), 2);
    let ready = run(&["task", "ready", &project_id]);
    assert_eq!(
        ready.as_array().unwrap().len(),
        1,
        "the dependent task waits"
    );
    assert_eq!(ready[0]["title"], "Add CSV export");
    assert!(run(&["project", "plan-show", &project_id]).is_null());
}

#[test]
fn parallel_capacity_is_set_and_validated_from_the_cli() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let project = cronos(&data, &["project", "create", "Parallel"]).unwrap();
    let project_id = id(&project);
    assert_eq!(project["max_parallel_tasks"], 1);

    let configured = cronos(&data, &["project", "set-parallelism", &project_id, "3"]).unwrap();
    assert_eq!(configured["max_parallel_tasks"], 3);
    let error = cronos(&data, &["project", "set-parallelism", &project_id, "9"]).unwrap_err();
    assert!(error.contains("between 1 and 8"), "{error}");
}
