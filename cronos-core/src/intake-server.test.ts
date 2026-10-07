import { expect, test } from "bun:test";
import { getQueueItem, listQueuedItems } from "cronos-queue/queue";
import { createIntakeServer } from "./intake-server";
import { openStorageDatabase } from "cronos-storage/database";

test("accepts valid work through the loopback intake and queues it", async () => {
  const db = await openStorageDatabase(":memory:");
  const server = createIntakeServer(db, 0);

  try {
    expect(new URL(server.url).hostname).toBe("127.0.0.1");
    const response = await fetch(new URL("/tasks", server.url), {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ description: "  Add task intake  " }),
    });
    const item = await response.json();

    expect(response.status).toBe(201);
    expect(item.status).toBe("queued");
    expect(item.description).toBe("Add task intake");
    expect(getQueueItem(db, item.id)?.description).toBe("Add task intake");
  } finally {
    server.stop(true);
    db.close(true);
  }
});

test("rejects malformed or incomplete requests without creating queue items", async () => {
  const db = await openStorageDatabase(":memory:");
  const server = createIntakeServer(db, 0);
  const endpoint = new URL("/tasks", server.url);

  try {
    const malformed = await fetch(endpoint, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: "{",
    });
    const missingDescription = await fetch(endpoint, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ title: "No description" }),
    });
    const emptyDescription = await fetch(endpoint, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ description: "  " }),
    });

    expect(malformed.status).toBe(400);
    expect(missingDescription.status).toBe(400);
    expect(emptyDescription.status).toBe(400);
    expect(listQueuedItems(db)).toHaveLength(0);
  } finally {
    server.stop(true);
    db.close(true);
  }
});

test("rejects oversized and non-JSON requests", async () => {
  const db = await openStorageDatabase(":memory:");
  const server = createIntakeServer(db, 0);
  const endpoint = new URL("/tasks", server.url);

  try {
    const oversized = await fetch(endpoint, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ description: "x".repeat(70 * 1024) }),
    });
    const wrongContentType = await fetch(endpoint, {
      method: "POST",
      headers: { "content-type": "text/plain" },
      body: "description",
    });

    expect(oversized.status).toBe(413);
    expect(wrongContentType.status).toBe(415);
    expect(listQueuedItems(db)).toHaveLength(0);
  } finally {
    server.stop(true);
    db.close(true);
  }
});
