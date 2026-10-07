import type { Database } from "bun:sqlite";

export type QueueItem = {
  id: string;
  description: string;
  status: "queued";
  createdAt: string;
  updatedAt: string;
};

export type NewQueueItem = {
  id: string;
  description: string;
};

type QueueItemRow = {
  id: string;
  description: string;
  status: "queued";
  created_at: string;
  updated_at: string;
};

function toQueueItem(row: QueueItemRow): QueueItem {
  return {
    id: row.id,
    description: row.description,
    status: row.status,
    createdAt: row.created_at,
    updatedAt: row.updated_at,
  };
}

export function saveQueueItem(db: Database, item: NewQueueItem): QueueItem {
  const now = new Date().toISOString();
  db.query(`
    INSERT INTO tasks (id, description, status, created_at, updated_at)
    VALUES (?, ?, 'queued', ?, ?)
  `).run(item.id, item.description, now, now);

  return {
    id: item.id,
    description: item.description,
    status: "queued",
    createdAt: now,
    updatedAt: now,
  };
}

export function getQueueItem(db: Database, id: string): QueueItem | null {
  const row = db.query<QueueItemRow, [string]>(`
    SELECT id, description, status, created_at, updated_at
    FROM tasks
    WHERE id = ?
  `).get(id);

  return row ? toQueueItem(row) : null;
}

export function listQueuedItems(db: Database): QueueItem[] {
  const rows = db.query<QueueItemRow, []>(`
    SELECT id, description, status, created_at, updated_at
    FROM tasks
    WHERE status = 'queued'
    ORDER BY created_at ASC, id ASC
  `).all();

  return rows.map(toQueueItem);
}
