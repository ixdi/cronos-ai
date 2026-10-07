import type { Database } from "bun:sqlite";
import { saveQueueItem } from "cronos-queue/queue";

const MAX_REQUEST_BYTES = 64 * 1024;

async function readBoundedBody(request: Request): Promise<string | null> {
  const declaredLength = Number(request.headers.get("content-length"));
  if (Number.isFinite(declaredLength) && declaredLength > MAX_REQUEST_BYTES) return null;

  const reader = request.body?.getReader();
  if (!reader) return "";

  const chunks: Uint8Array[] = [];
  let totalBytes = 0;

  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      totalBytes += value.byteLength;
      if (totalBytes > MAX_REQUEST_BYTES) {
        await reader.cancel();
        return null;
      }
      chunks.push(value);
    }
  } finally {
    reader.releaseLock();
  }

  const body = new Uint8Array(totalBytes);
  let offset = 0;
  for (const chunk of chunks) {
    body.set(chunk, offset);
    offset += chunk.byteLength;
  }

  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(body);
  } catch {
    return "";
  }
}

export function createIntakeServer(db: Database, port = 3000) {
  return Bun.serve({
    hostname: "127.0.0.1",
    port,
    routes: {
      "/tasks": {
        POST: async (request) => {
          const contentType = request.headers.get("content-type")
            ?.split(";")[0]
            .trim()
            .toLowerCase();
          if (contentType !== "application/json") {
            return Response.json({ error: "Content-Type must be application/json" }, { status: 415 });
          }

          const text = await readBoundedBody(request);
          if (text === null) {
            return Response.json({ error: "Request body is too large" }, { status: 413 });
          }

          let body: unknown;
          try {
            body = JSON.parse(text);
          } catch {
            return Response.json({ error: "Request body must be valid JSON" }, { status: 400 });
          }

          if (
            typeof body !== "object" ||
            body === null ||
            Array.isArray(body) ||
            typeof (body as { description?: unknown }).description !== "string" ||
            !(body as { description: string }).description.trim()
          ) {
            return Response.json({ error: "A non-empty description is required" }, { status: 400 });
          }

          try {
            const item = saveQueueItem(db, {
              id: crypto.randomUUID(),
              description: (body as { description: string }).description.trim(),
            });
            return Response.json(item, { status: 201 });
          } catch {
            return Response.json({ error: "Could not queue task" }, { status: 500 });
          }
        },
      },
    },
    fetch: () => Response.json({ error: "Not found" }, { status: 404 }),
  });
}
