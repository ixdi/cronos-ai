import { expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { openStorageDatabase } from "cronos-storage/database";
import { getQueueItem, listQueuedItems, saveQueueItem } from "./queue";

test("saves and retrieves a queued item", async () => {
  const db = await openStorageDatabase(":memory:");

  try {
    const created = saveQueueItem(db, { id: "task-1", description: "Add a feature" });

    expect(created.status).toBe("queued");
    expect(getQueueItem(db, "task-1")).toEqual(created);
    expect(getQueueItem(db, "missing")).toBeNull();
  } finally {
    db.close(true);
  }
});

test("lists queued items in deterministic creation order", async () => {
  const db = await openStorageDatabase(":memory:");

  try {
    saveQueueItem(db, { id: "task-b", description: "Second" });
    saveQueueItem(db, { id: "task-a", description: "First" });
    db.query("UPDATE tasks SET created_at = ? WHERE id = ?")
      .run("2026-01-02T00:00:00.000Z", "task-b");
    db.query("UPDATE tasks SET created_at = ? WHERE id = ?")
      .run("2026-01-01T00:00:00.000Z", "task-a");

    expect(listQueuedItems(db).map(({ id }) => id)).toEqual(["task-a", "task-b"]);
  } finally {
    db.close(true);
  }
});

test("does not create a duplicate item for an existing identifier", async () => {
  const db = await openStorageDatabase(":memory:");

  try {
    saveQueueItem(db, { id: "task-1", description: "Original" });
    expect(() => saveQueueItem(db, { id: "task-1", description: "Duplicate" })).toThrow();
    expect(listQueuedItems(db)).toHaveLength(1);
    expect(getQueueItem(db, "task-1")?.description).toBe("Original");
  } finally {
    db.close(true);
  }
});

test("queued items survive closing and reopening the SQLite database", async () => {
  const directory = mkdtempSync(join(tmpdir(), "cronos-queue-"));
  const databasePath = join(directory, "queue.sqlite");
  let db = await openStorageDatabase(databasePath);

  try {
    const item = saveQueueItem(db, { id: "task-persistent", description: "Persist me" });
    db.close(true);
    db = await openStorageDatabase(databasePath);

    expect(getQueueItem(db, item.id)).toEqual(item);
    expect(listQueuedItems(db)).toEqual([item]);
  } finally {
    db.close(true);
    rmSync(directory, { recursive: true, force: true });
  }
});
