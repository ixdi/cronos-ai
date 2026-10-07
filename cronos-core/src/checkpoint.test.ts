import { expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Annotation, END, START, StateGraph } from "@langchain/langgraph";
import { saveQueueItem } from "cronos-queue/queue";
import { BunSqliteCheckpointer } from "cronos-storage/checkpointer";
import { openStorageDatabase } from "cronos-storage/database";

const Counter = Annotation.Root({
  count: Annotation<number>({
    reducer: (_current, update) => update,
    default: () => 0,
  }),
});

function buildGraph(checkpointer: BunSqliteCheckpointer) {
  return new StateGraph(Counter)
    .addNode("increment", (state) => ({ count: state.count + 1 }))
    .addEdge(START, "increment")
    .addEdge("increment", END)
    .compile({ checkpointer });
}

test("restores LangGraph state after closing and reopening SQLite", async () => {
  const directory = mkdtempSync(join(tmpdir(), "cronos-langgraph-"));
  const databasePath = join(directory, "workflow.sqlite");
  let db = await openStorageDatabase(databasePath);

  try {
    saveQueueItem(db, { id: "task-graph", description: "Persist graph state" });
    const config = { configurable: { thread_id: "task-graph" } };
    const firstGraph = buildGraph(new BunSqliteCheckpointer(db));
    const result = await firstGraph.invoke({ count: 4 }, config);
    expect(result.count).toBe(5);
    db.close(true);

    db = await openStorageDatabase(databasePath);
    const restoredGraph = buildGraph(new BunSqliteCheckpointer(db));
    const snapshot = await restoredGraph.getState(config);
    expect(snapshot.values.count).toBe(5);
    expect(snapshot.next).toEqual([]);
  } finally {
    db.close(true);
    rmSync(directory, { recursive: true, force: true });
  }
});
