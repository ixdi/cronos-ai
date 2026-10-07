import { Database } from "bun:sqlite";

const migrations = [
  {
    version: 1,
    file: new URL("../migrations/001_initial.sql", import.meta.url),
  },
  {
    version: 2,
    file: new URL("../migrations/002_single_active_task.sql", import.meta.url),
  },
  {
    version: 3,
    file: new URL("../migrations/003_single_inflight_workflow.sql", import.meta.url),
  },
  {
    version: 4,
    file: new URL("../migrations/004_agent_run_events.sql", import.meta.url),
  },
  {
    version: 5,
    file: new URL("../migrations/005_workflow_workspace.sql", import.meta.url),
  },
  {
    version: 6,
    file: new URL("../migrations/006_pull_request_delivery.sql", import.meta.url),
  },
] as const;

export async function migrateStorageDatabase(db: Database): Promise<void> {
  db.exec("PRAGMA foreign_keys = ON;");
  db.exec(`
    CREATE TABLE IF NOT EXISTS schema_migrations (
      version INTEGER PRIMARY KEY,
      applied_at TEXT NOT NULL
    );
  `);

  const recordMigration = db.transaction((version: number, sql: string) => {
    db.exec(sql);
    db.query("INSERT INTO schema_migrations (version, applied_at) VALUES (?, ?)")
      .run(version, new Date().toISOString());
  });

  for (const migration of migrations) {
    const applied = db.query(
      "SELECT version FROM schema_migrations WHERE version = ?",
    ).get(migration.version);

    if (applied) continue;

    const sql = await Bun.file(migration.file).text();
    recordMigration(migration.version, sql);
  }
}

export async function openStorageDatabase(path: string): Promise<Database> {
  const db = new Database(path);

  try {
    await migrateStorageDatabase(db);
    return db;
  } catch (error) {
    db.close(true);
    throw error;
  }
}
