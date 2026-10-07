import { expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { BunSqliteCheckpointer } from "./bun-sqlite-checkpointer";
import { openStorageDatabase } from "./database";

const checkpoint = (id: string, value: string) => ({
  v: 4,
  id,
  ts: `2026-01-01T00:00:0${id}.000Z`,
  channel_values: { value },
  channel_versions: { value: Number(id) },
  versions_seen: {},
});

async function createTask(db: Awaited<ReturnType<typeof openStorageDatabase>>) {
  db.query(`
    INSERT INTO tasks (id, description, status, created_at, updated_at)
    VALUES (?, ?, ?, ?, ?)
  `).run("task-1", "Checkpoint test", "active", "2026-01-01T00:00:00.000Z", "2026-01-01T00:00:00.000Z");
}

test("round-trips checkpoints and pending writes", async () => {
  const db = await openStorageDatabase(":memory:");

  try {
    await createTask(db);
    const saver = new BunSqliteCheckpointer(db);
    const config = { configurable: { thread_id: "task-1" } };
    const savedConfig = await saver.put(
      config,
      checkpoint("1", "saved-value"),
      { source: "input", step: -1, parents: {} },
      { value: 1 },
    );
    await saver.putWrites(savedConfig, [["result", "pending-value"]], "agent-1");

    const tuple = await saver.getTuple(savedConfig);
    expect(tuple?.checkpoint.channel_values.value).toBe("saved-value");
    expect(tuple?.metadata?.step).toBe(-1);
    expect(tuple?.pendingWrites).toEqual([["agent-1", "result", "pending-value"]]);
  } finally {
    db.close(true);
  }
});

test("loads a persisted checkpoint after closing and reopening SQLite", async () => {
  const directory = mkdtempSync(join(tmpdir(), "cronos-checkpoint-"));
  const databasePath = join(directory, "workflow.sqlite");
  let db = await openStorageDatabase(databasePath);

  try {
    await createTask(db);
    const firstSaver = new BunSqliteCheckpointer(db);
    const config = await firstSaver.put(
      { configurable: { thread_id: "task-1" } },
      checkpoint("1", "before-restart"),
      { source: "loop", step: 0, parents: {} },
      { value: 1 },
    );
    db.close(true);

    db = await openStorageDatabase(databasePath);
    const restored = await new BunSqliteCheckpointer(db).getTuple(config);
    expect(restored?.checkpoint.channel_values.value).toBe("before-restart");
  } finally {
    db.close(true);
    rmSync(directory, { recursive: true, force: true });
  }
});
