import { expect, test } from "bun:test";
import { Database } from "bun:sqlite";
import { migrateStorageDatabase, openStorageDatabase } from "./database";

test("applies the initial schema to an empty database", async () => {
  const db = await openStorageDatabase(":memory:");

  try {
    const tables = db.query<{ name: string }, []>(
      "SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name",
    ).all().map(({ name }) => name);

    expect(tables).toContain("tasks");
    expect(tables).toContain("workflows");
    expect(tables).toContain("agent_runs");
    expect(tables).toContain("workflow_checkpoints");
    expect(tables).toContain("workflow_checkpoint_writes");
    expect(tables).toContain("schema_migrations");

    const migration = db.query<{ version: number }, []>(
      "SELECT version FROM schema_migrations ORDER BY version DESC LIMIT 1",
    ).get();
    expect(migration?.version).toBe(6);
  } finally {
    db.close(true);
  }
});

test("reapplying migrations is idempotent", async () => {
  const db = new Database(":memory:");

  try {
    await migrateStorageDatabase(db);
    await migrateStorageDatabase(db);

    const migrations = db.query<{ count: number }, []>(
      "SELECT COUNT(*) AS count FROM schema_migrations",
    ).get();
    expect(migrations?.count).toBe(6);
  } finally {
    db.close(true);
  }
});

test("stores checkpoint data linked to a task", async () => {
  const db = await openStorageDatabase(":memory:");

  try {
    db.query(`
      INSERT INTO tasks (id, description, status, created_at, updated_at)
      VALUES (?, ?, ?, ?, ?)
    `).run("task-1", "Example", "active", "2026-01-01T00:00:00.000Z", "2026-01-01T00:00:00.000Z");

    const checkpoint = Buffer.from("checkpoint-state");
    db.query(`
      INSERT INTO workflow_checkpoints (
        thread_id, checkpoint_ns, checkpoint_id, checkpoint_type,
        checkpoint, metadata_type, metadata, created_at
      ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
    `).run(
      "task-1",
      "",
      "checkpoint-1",
      "json",
      checkpoint,
      "json",
      Buffer.from("{}"),
      "2026-01-01T00:00:00.000Z",
    );

    const stored = db.query<{ checkpoint: Uint8Array }, [string, string, string]>(
      `SELECT checkpoint FROM workflow_checkpoints
       WHERE thread_id = ? AND checkpoint_ns = ? AND checkpoint_id = ?`,
    ).get("task-1", "", "checkpoint-1");

    expect(stored).toBeDefined();
    expect(Buffer.from(stored!.checkpoint).toString()).toBe("checkpoint-state");
  } finally {
    db.close(true);
  }
});

test("enforces one in-flight workflow across active and review-pending states", async () => {
  const db = await openStorageDatabase(":memory:");
  const now = new Date().toISOString();

  try {
    db.query(`
      INSERT INTO tasks (id, description, status, created_at, updated_at)
      VALUES (?, ?, 'active', ?, ?)
    `).run("active-1", "First active task", now, now);
    db.query(`
      INSERT INTO tasks (id, description, status, created_at, updated_at)
      VALUES (?, ?, 'queued', ?, ?)
    `).run("queued-2", "Queued task", now, now);

    expect(() => db.query("UPDATE tasks SET status = 'review_pending' WHERE id = 'queued-2'").run()).toThrow();
  } finally {
    db.close(true);
  }
});

test("enables foreign keys and rejects invalid task state", async () => {
  const db = await openStorageDatabase(":memory:");

  try {
    const foreignKeys = db.query<{ foreign_keys: number }, []>(
      "PRAGMA foreign_keys",
    ).get();
    expect(foreignKeys?.foreign_keys).toBe(1);

    expect(() => db.query(`
      INSERT INTO tasks (id, description, status, created_at, updated_at)
      VALUES ('task-1', 'Example', 'unknown', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')
    `).run()).toThrow();
  } finally {
    db.close(true);
  }
});
