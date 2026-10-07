import { expect, test } from "bun:test";
import { openStorageDatabase } from "./database";
import { inspectTask, listTaskSummaries } from "./task-inspection";

test("lists task states with persisted workflow stages and respects the one-in-flight constraint", async () => {
  const db = await openStorageDatabase(":memory:");
  const createdAt = (day: number) => `2026-01-0${day}T00:00:00.000Z`;
  try {
    const insertTask = (id: string, status: string, day: number, workflow?: {
      status: string;
      stage: string;
      error?: string;
    }) => {
      const timestamp = createdAt(day);
      db.query(`
        INSERT INTO tasks (id, description, status, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?)
      `).run(id, `Description for ${status}`, status, timestamp, timestamp);
      if (workflow) {
        db.query(`
          INSERT INTO workflows (task_id, status, current_stage, last_error, created_at, updated_at)
          VALUES (?, ?, ?, ?, ?, ?)
        `).run(id, workflow.status, workflow.stage, workflow.error ?? null, timestamp, timestamp);
      }
    };

    insertTask("task-queued", "queued", 1);
    insertTask("task-active", "active", 2, { status: "running", stage: "implementation" });
    insertTask("task-blocked", "blocked", 3, { status: "blocked", stage: "blocked" });
    insertTask("task-errored", "errored", 4, { status: "errored", stage: "errored" });
    insertTask("task-delivered", "delivered", 5, { status: "completed", stage: "delivered" });

    db.query(`
      UPDATE workflows
      SET pull_request_number = 11,
          pull_request_url = 'https://github.com/example/repo/pull/11',
          pull_request_branch = 'cronos/task-delivered'
      WHERE task_id = 'task-delivered'
    `).run();

    const activeView = listTaskSummaries(db);
    expect(activeView.map((task) => task.status)).toEqual([
      "queued", "active", "blocked", "errored", "delivered",
    ]);
    expect(activeView[0]?.workflow).toBeNull();
    expect(activeView.find((task) => task.status === "active")?.workflow?.currentStage).toBe("implementation");
    expect(activeView.find((task) => task.status === "delivered")?.workflow?.pullRequest).toEqual({
      number: 11,
      url: "https://github.com/example/repo/pull/11",
      branch: "cronos/task-delivered",
    });

    db.query("UPDATE tasks SET status = 'interrupted' WHERE id = 'task-active'").run();
    db.query(`
      UPDATE workflows SET status = 'interrupted', current_stage = 'verification', last_error = 'Restarted'
      WHERE task_id = 'task-active'
    `).run();
    expect(listTaskSummaries(db, "interrupted")).toMatchObject([
      { id: "task-active", status: "interrupted", workflow: { currentStage: "verification", lastError: "Restarted" } },
    ]);

    db.query("UPDATE tasks SET status = 'review_pending' WHERE id = 'task-active'").run();
    db.query(`
      UPDATE workflows SET status = 'running', current_stage = 'final_review', last_error = NULL
      WHERE task_id = 'task-active'
    `).run();
    expect(listTaskSummaries(db, "review_pending")).toMatchObject([
      { id: "task-active", status: "review_pending", workflow: { currentStage: "final_review" } },
    ]);
  } finally {
    db.close(true);
  }
});

test("inspects latest code-review and verification output", async () => {
  const db = await openStorageDatabase(":memory:");
  const createdAt = "2026-01-01T00:00:00.000Z";
  try {
    db.query(`
      INSERT INTO tasks (id, description, status, created_at, updated_at)
      VALUES ('task-review', 'Review outputs', 'review_pending', ?, ?)
    `).run(createdAt, createdAt);
    db.query(`
      INSERT INTO workflows (task_id, status, current_stage, last_error, created_at, updated_at)
      VALUES ('task-review', 'running', 'final_review', NULL, ?, ?)
    `).run(createdAt, createdAt);
    const insertAgentRun = db.query(`
      INSERT INTO agent_runs (id, task_id, role, status, result, started_at, finished_at, created_at)
      VALUES (?, 'task-review', ?, 'succeeded', ?, ?, ?, ?)
    `);
    insertAgentRun.run(
      "run-review",
      "code-review",
      JSON.stringify({ summary: "No blocking issues", changedFiles: ["src/a.ts"], structuredResult: { status: "passed", summary: "No blocking issues" } }),
      createdAt,
      createdAt,
      createdAt,
    );
    insertAgentRun.run(
      "run-verification",
      "verification",
      JSON.stringify({ summary: "Unit tests passed", changedFiles: [], structuredResult: { status: "passed", summary: "Unit tests passed" } }),
      createdAt,
      createdAt,
      createdAt,
    );

    const task = inspectTask(db, "task-review");
    expect(task?.status).toBe("review_pending");
    expect(task?.workflow?.currentStage).toBe("final_review");
    expect(task?.codeReview).toMatchObject({
      summary: "No blocking issues",
      changedFiles: ["src/a.ts"],
      structuredResult: { status: "passed", summary: "No blocking issues" },
    });
    expect(task?.verification).toMatchObject({
      summary: "Unit tests passed",
      structuredResult: { status: "passed", summary: "Unit tests passed" },
    });
    expect(inspectTask(db, "missing-task")).toBeNull();
  } finally {
    db.close(true);
  }
});
