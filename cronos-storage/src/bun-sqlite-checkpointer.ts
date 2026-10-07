import { Database } from "bun:sqlite";
import {
  BaseCheckpointSaver,
  WRITES_IDX_MAP,
  type ChannelVersions,
  type Checkpoint,
  type CheckpointListOptions,
  type CheckpointMetadata,
  type CheckpointPendingWrite,
  type CheckpointTuple,
  type PendingWrite,
} from "@langchain/langgraph-checkpoint";
import type { RunnableConfig } from "@langchain/core/runnables";

type CheckpointRow = {
  thread_id: string;
  checkpoint_ns: string;
  checkpoint_id: string;
  parent_checkpoint_id: string | null;
  checkpoint_type: string;
  checkpoint: Uint8Array;
  metadata_type: string;
  metadata: Uint8Array;
};

type WriteRow = {
  writer_task_id: string;
  channel: string;
  value_type: string;
  value: Uint8Array;
};

export class BunSqliteCheckpointer extends BaseCheckpointSaver {
  constructor(private readonly db: Database) {
    super();
  }

  async getTuple(config: RunnableConfig): Promise<CheckpointTuple | undefined> {
    const threadId = config.configurable?.thread_id;
    if (typeof threadId !== "string" || !threadId) return undefined;

    const checkpointNs = String(config.configurable?.checkpoint_ns ?? "");
    const checkpointId = config.configurable?.checkpoint_id;
    const row = typeof checkpointId === "string"
      ? this.db.query<CheckpointRow, [string, string, string]>(`
          SELECT * FROM workflow_checkpoints
          WHERE thread_id = ? AND checkpoint_ns = ? AND checkpoint_id = ?
        `).get(threadId, checkpointNs, checkpointId)
      : this.db.query<CheckpointRow, [string, string]>(`
          SELECT * FROM workflow_checkpoints
          WHERE thread_id = ? AND checkpoint_ns = ?
          ORDER BY checkpoint_id DESC LIMIT 1
        `).get(threadId, checkpointNs);

    if (!row) return undefined;

    const checkpoint = await this.serde.loadsTyped(row.checkpoint_type, row.checkpoint) as Checkpoint;
    const metadata = await this.serde.loadsTyped(row.metadata_type, row.metadata) as CheckpointMetadata;
    const writes = this.db.query<WriteRow, [string, string, string]>(`
      SELECT writer_task_id, channel, value_type, value
      FROM workflow_checkpoint_writes
      WHERE thread_id = ? AND checkpoint_ns = ? AND checkpoint_id = ?
      ORDER BY writer_task_id, write_index
    `).all(row.thread_id, row.checkpoint_ns, row.checkpoint_id);
    const pendingWrites = await Promise.all(writes.map(async (write) => [
      write.writer_task_id,
      write.channel,
      await this.serde.loadsTyped(write.value_type, write.value),
    ] as CheckpointPendingWrite));

    const resultConfig: RunnableConfig = {
      configurable: {
        ...config.configurable,
        thread_id: row.thread_id,
        checkpoint_ns: row.checkpoint_ns,
        checkpoint_id: row.checkpoint_id,
      },
    };
    const tuple: CheckpointTuple = { config: resultConfig, checkpoint, metadata, pendingWrites };

    if (row.parent_checkpoint_id) {
      tuple.parentConfig = {
        configurable: {
          thread_id: row.thread_id,
          checkpoint_ns: row.checkpoint_ns,
          checkpoint_id: row.parent_checkpoint_id,
        },
      };
    }

    return tuple;
  }

  async *list(
    config: RunnableConfig,
    options?: CheckpointListOptions,
  ): AsyncGenerator<CheckpointTuple> {
    const rows = this.db.query<CheckpointRow, []>(`
      SELECT * FROM workflow_checkpoints
      ORDER BY thread_id, checkpoint_ns, checkpoint_id DESC
    `).all();
    const threadId = config.configurable?.thread_id;
    const checkpointNs = config.configurable?.checkpoint_ns;
    const checkpointId = config.configurable?.checkpoint_id;
    const beforeId = options?.before?.configurable?.checkpoint_id;
    let remaining = options?.limit ?? Number.POSITIVE_INFINITY;

    for (const row of rows) {
      if (threadId !== undefined && row.thread_id !== threadId) continue;
      if (checkpointNs !== undefined && row.checkpoint_ns !== checkpointNs) continue;
      if (checkpointId !== undefined && row.checkpoint_id !== checkpointId) continue;
      if (typeof beforeId === "string" && row.checkpoint_id >= beforeId) continue;
      if (remaining <= 0) return;

      const tuple = await this.getTuple({
        configurable: {
          thread_id: row.thread_id,
          checkpoint_ns: row.checkpoint_ns,
          checkpoint_id: row.checkpoint_id,
        },
      });
      if (!tuple) continue;

      if (options?.filter && !Object.entries(options.filter).every(
        ([key, value]) => (tuple.metadata as Record<string, unknown>)[key] === value,
      )) continue;

      yield tuple;
      remaining -= 1;
    }
  }

  async put(
    config: RunnableConfig,
    checkpoint: Checkpoint,
    metadata: CheckpointMetadata,
    _newVersions: ChannelVersions,
  ): Promise<RunnableConfig> {
    const threadId = config.configurable?.thread_id;
    if (typeof threadId !== "string" || !threadId) {
      throw new Error("Checkpoint config must include configurable.thread_id");
    }

    const checkpointNs = String(config.configurable?.checkpoint_ns ?? "");
    const [checkpointType, checkpointBytes] = await this.serde.dumpsTyped(checkpoint);
    const [metadataType, metadataBytes] = await this.serde.dumpsTyped(metadata);
    const parentId = typeof config.configurable?.checkpoint_id === "string"
      ? config.configurable.checkpoint_id
      : null;

    this.db.query(`
      INSERT INTO workflow_checkpoints (
        thread_id, checkpoint_ns, checkpoint_id, parent_checkpoint_id,
        checkpoint_type, checkpoint, metadata_type, metadata, created_at
      ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
    `).run(
      threadId,
      checkpointNs,
      checkpoint.id,
      parentId,
      checkpointType,
      checkpointBytes,
      metadataType,
      metadataBytes,
      checkpoint.ts,
    );

    return {
      configurable: {
        ...config.configurable,
        thread_id: threadId,
        checkpoint_ns: checkpointNs,
        checkpoint_id: checkpoint.id,
      },
    };
  }

  async putWrites(config: RunnableConfig, writes: PendingWrite[], taskId: string): Promise<void> {
    const threadId = config.configurable?.thread_id;
    const checkpointId = config.configurable?.checkpoint_id;
    if (typeof threadId !== "string" || !threadId || typeof checkpointId !== "string") {
      throw new Error("Checkpoint write config must include thread_id and checkpoint_id");
    }

    const checkpointNs = String(config.configurable?.checkpoint_ns ?? "");
    const serialized = await Promise.all(writes.map(async ([channel, value], index) => {
      const [valueType, valueBytes] = await this.serde.dumpsTyped(value);
      return {
        taskId,
        index: WRITES_IDX_MAP[channel] ?? index,
        channel,
        valueType,
        valueBytes,
      };
    }));

    const insert = this.db.query(`
      INSERT INTO workflow_checkpoint_writes (
        thread_id, checkpoint_ns, checkpoint_id, writer_task_id,
        write_index, channel, value_type, value
      ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
      ON CONFLICT (thread_id, checkpoint_ns, checkpoint_id, writer_task_id, write_index)
      DO UPDATE SET channel = excluded.channel, value_type = excluded.value_type, value = excluded.value
      WHERE excluded.write_index < 0
    `);

    const saveWrites = this.db.transaction(() => {
      for (const write of serialized) {
        insert.run(
          threadId,
          checkpointNs,
          checkpointId,
          write.taskId,
          write.index,
          write.channel,
          write.valueType,
          write.valueBytes,
        );
      }
    });
    saveWrites();
  }

  async deleteThread(threadId: string): Promise<void> {
    this.db.query("DELETE FROM workflow_checkpoints WHERE thread_id = ?").run(threadId);
  }
}
