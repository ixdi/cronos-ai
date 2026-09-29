"""Generate a Pi extension that bridges profile-allowlisted MCP stdio tools."""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path

from cronos_ai.profiles import (
    ResolvedSpecialistProfile,
    profile_resource_manifest,
)


class MCPBridgeError(RuntimeError):
    """Raised when profile-scoped MCP bridge files cannot be prepared."""


@dataclass(frozen=True)
class MCPBridgeFiles:
    """Temporary files used to load the curated MCP extension in Pi."""

    extension_path: Path
    config_path: Path
    tool_names: tuple[str, ...]


PI_MCP_BRIDGE_EXTENSION = r'''import { readFileSync } from "node:fs";
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { Type } from "typebox";

const MAX_LINE_BYTES = 4 * 1024 * 1024;
const MAX_TOOL_COUNT = 256;
const MAX_TOOL_SCHEMA_BYTES = 256 * 1024;
const MAX_TOOL_RESULT_BYTES = 1024 * 1024;
const PROTOCOL_VERSIONS = new Set([
  "2025-11-25",
  "2025-06-18",
  "2025-03-26",
  "2024-11-05",
]);
const configPath = process.env.FACTORY_MCP_CONFIG_PATH;
if (!configPath) throw new Error("Factory MCP profile configuration is missing");
const config = JSON.parse(readFileSync(configPath, "utf8"));

function childEnvironment() {
  const env = {};
  for (const key of [
    "PATH",
    "HOME",
    "TMPDIR",
    "LANG",
    "LC_ALL",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "NODE_USE_ENV_PROXY",
  ]) {
    if (process.env[key]) env[key] = process.env[key];
  }
  return env;
}

function sanitizeSchema(schema, depth = 0, budget = { count: 0 }) {
  if (depth > 16 || ++budget.count > 2048) {
    throw new Error("MCP input schema exceeds complexity limit");
  }
  if (!schema || typeof schema !== "object" || Array.isArray(schema)) {
    throw new Error("MCP input schema must be a JSON Schema object");
  }
  const allowed = new Set([
    "type",
    "properties",
    "required",
    "items",
    "additionalProperties",
    "enum",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "minLength",
    "maxLength",
    "minItems",
    "maxItems",
    "$schema",
    "title",
    "description",
    "default",
  ]);
  for (const key of Object.keys(schema)) {
    if (!allowed.has(key)) {
      throw new Error(`Unsupported MCP schema keyword: ${key}`);
    }
  }
  if (schema.type === "object") {
    const properties = schema.properties ?? {};
    if (typeof properties !== "object" || Array.isArray(properties)) {
      throw new Error("MCP object properties must be an object");
    }
    const safeProperties = Object.fromEntries(
      Object.entries(properties).map(([name, child]) => [
        name,
        sanitizeSchema(child, depth + 1, budget),
      ]),
    );
    const required = schema.required ?? [];
    if (
      !Array.isArray(required) ||
      !required.every((name) => typeof name === "string" && name in safeProperties)
    ) {
      throw new Error("MCP required properties are invalid");
    }
    const additional = schema.additionalProperties;
    if (additional !== undefined && typeof additional !== "boolean") {
      throw new Error("MCP additionalProperties must be boolean");
    }
    return {
      type: "object",
      properties: safeProperties,
      required,
      additionalProperties: additional ?? false,
    };
  }
  if (schema.type === "array") {
    if (!schema.items) throw new Error("MCP array schema must define items");
    return {
      type: "array",
      items: sanitizeSchema(schema.items, depth + 1, budget),
      ...(Number.isInteger(schema.minItems) ? { minItems: schema.minItems } : {}),
      ...(Number.isInteger(schema.maxItems) ? { maxItems: schema.maxItems } : {}),
    };
  }
  if (!["string", "number", "integer", "boolean"].includes(schema.type)) {
    throw new Error("MCP schema uses an unsupported or missing type");
  }
  for (const key of [
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
  ]) {
    if (
      schema[key] !== undefined &&
      (typeof schema[key] !== "number" || !Number.isFinite(schema[key]))
    ) {
      throw new Error(`MCP numeric constraint is invalid: ${key}`);
    }
  }
  for (const key of ["minLength", "maxLength", "minItems", "maxItems"]) {
    if (
      schema[key] !== undefined &&
      (!Number.isInteger(schema[key]) || schema[key] < 0 || schema[key] > 1000000)
    ) {
      throw new Error(`MCP size constraint is invalid: ${key}`);
    }
  }
  if (schema.enum !== undefined) {
    if (
      !Array.isArray(schema.enum) ||
      schema.enum.length > 256 ||
      !schema.enum.every((item) =>
        ["string", "number", "boolean"].includes(typeof item),
      )
    ) {
      throw new Error("MCP schema enum is invalid");
    }
  }
  const result = { type: schema.type };
  for (const key of [
    "enum",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "minLength",
    "maxLength",
  ]) {
    if (schema[key] !== undefined) result[key] = schema[key];
  }
  return result;
}

function toolContent(result) {
  const blocks = Array.isArray(result?.content) ? result.content : [];
  const content = [];
  for (const block of blocks) {
    if (block?.type === "text" && typeof block.text === "string") {
      content.push({ type: "text", text: block.text });
    } else if (
      block?.type === "image" &&
      typeof block.data === "string" &&
      typeof block.mimeType === "string" &&
      block.data.length <= MAX_TOOL_RESULT_BYTES
    ) {
      content.push({ type: "image", data: block.data, mimeType: block.mimeType });
    } else {
      content.push({ type: "text", text: JSON.stringify(block).slice(0, 8192) });
    }
  }
  let size = 0;
  for (const block of content) {
    size += JSON.stringify(block).length;
    if (size > MAX_TOOL_RESULT_BYTES) {
      throw new Error("MCP tool result exceeds size limit");
    }
  }
  if (content.length === 0 && result?.structuredContent !== undefined) {
    const serialized = JSON.stringify(result.structuredContent);
    if (serialized.length > MAX_TOOL_RESULT_BYTES) {
      throw new Error("MCP structured result exceeds size limit");
    }
    content.push({ type: "text", text: serialized });
  }
  return content.length
    ? content
    : [{ type: "text", text: "MCP tool returned no content" }];
}

class StdioMcpClient {
  constructor(server) {
    this.server = server;
    this.nextId = 0;
    this.pending = new Map();
    this.child = spawn(server.command, server.arguments, {
      cwd: process.cwd(),
      env: childEnvironment(),
      shell: false,
      stdio: ["pipe", "pipe", "ignore"],
    });
    const lines = createInterface({ input: this.child.stdout, crlfDelay: Infinity });
    lines.on("line", (line) => this.onLine(line));
    this.child.on("error", () => this.failAll("MCP server process failed"));
    this.child.on("exit", () => this.failAll("MCP server exited"));
  }

  failAll(message) {
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timer);
      pending.reject(new Error(message));
    }
    this.pending.clear();
  }

  onLine(line) {
    if (Buffer.byteLength(line, "utf8") > MAX_LINE_BYTES) {
      this.failAll("MCP server record exceeded size limit");
      this.child.kill("SIGTERM");
      return;
    }
    let message;
    try {
      message = JSON.parse(line);
    } catch {
      this.failAll("MCP server emitted malformed JSON-RPC");
      this.child.kill("SIGTERM");
      return;
    }
    if (message && typeof message.method === "string" && message.id !== undefined) {
      this.child.stdin.write(
        JSON.stringify({
          jsonrpc: "2.0",
          id: message.id,
          error: { code: -32601, message: "Client requests are not supported" },
        }) + "\n",
      );
      return;
    }
    const pending = this.pending.get(message?.id);
    if (!pending) return;
    clearTimeout(pending.timer);
    this.pending.delete(message.id);
    if (message.error) {
      pending.reject(new Error("MCP server rejected a request"));
    } else {
      pending.resolve(message.result);
    }
  }

  request(method, params, timeoutMs = 30000) {
    const id = ++this.nextId;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error("MCP server request timed out"));
      }, timeoutMs);
      this.pending.set(id, { resolve, reject, timer });
      this.child.stdin.write(
        JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n",
        (error) => {
          if (error) {
            clearTimeout(timer);
            this.pending.delete(id);
            reject(new Error("MCP server request could not be written"));
          }
        },
      );
    });
  }

  notify(method, params = {}) {
    this.child.stdin.write(JSON.stringify({ jsonrpc: "2.0", method, params }) + "\n");
  }

  async initialize() {
    const result = await this.request("initialize", {
      protocolVersion: "2025-11-25",
      capabilities: {},
      clientInfo: { name: "cronos-ai", version: "0.1.0" },
    });
    if (!PROTOCOL_VERSIONS.has(result?.protocolVersion)) {
      throw new Error("MCP server selected an unsupported protocol version");
    }
    this.notify("notifications/initialized");
    const allTools = [];
    let cursor;
    const seenCursors = new Set();
    do {
      const result = await this.request("tools/list", cursor ? { cursor } : {});
      if (!Array.isArray(result?.tools)) {
        throw new Error("MCP tools/list returned an invalid result");
      }
      allTools.push(...result.tools);
      if (allTools.length > MAX_TOOL_COUNT) {
        throw new Error("MCP server exposed too many tools");
      }
      cursor = result.nextCursor;
      if (cursor !== undefined) {
        if (typeof cursor !== "string" || seenCursors.has(cursor)) {
          throw new Error("MCP tools/list returned an invalid pagination cursor");
        }
        seenCursors.add(cursor);
      }
    } while (cursor !== undefined);
    return allTools;
  }

  callTool(name, args) {
    return this.request("tools/call", { name, arguments: args }, 120000);
  }

  close() {
    try {
      this.child.stdin.end();
      this.child.kill("SIGTERM");
    } catch {
      // The MCP child may already have exited.
    }
  }
}

export default function factoryMcpBridge(pi) {
  const clients = [];
  const activeToolNames = [];
  const config = JSON.parse(readFileSync(process.env.FACTORY_MCP_CONFIG_PATH, "utf8"));
  pi.on("session_start", async (_event, ctx) => {
    try {
      for (const server of config.servers) {
        const client = new StdioMcpClient(server);
        clients.push(client);
        const discovered = await client.initialize();
        if (
          discovered.length > MAX_TOOL_COUNT ||
          discovered.some((tool) => typeof tool?.name !== "string")
        ) {
          throw new Error("MCP server returned an invalid tool catalog");
        }
        const byName = new Map(discovered.map((tool) => [tool.name, tool]));
        if (byName.size !== discovered.length) {
          throw new Error("MCP server returned duplicate tool names");
        }
        for (const selected of server.tools) {
          const tool = byName.get(selected.name);
          if (!tool || !tool.inputSchema || typeof tool.inputSchema !== "object") {
            throw new Error("MCP server is missing an approved tool or schema");
          }
          const schemaText = JSON.stringify(tool.inputSchema);
          if (Buffer.byteLength(schemaText, "utf8") > MAX_TOOL_SCHEMA_BYTES) {
            throw new Error("MCP tool schema exceeds size limit");
          }
          const inputSchema = sanitizeSchema(tool.inputSchema);
          pi.registerTool({
            name: selected.pi_tool_name,
            label: `MCP ${server.server_id}/${selected.name}`,
            description:
              `Factory-approved MCP tool ${server.server_id}/${selected.name}. ` +
              "Use only for its configured purpose.",
            parameters: Type.Unsafe(inputSchema),
            async execute(_toolCallId, params) {
              const result = await client.callTool(selected.name, params);
              return {
                content: toolContent(result),
                details: { serverId: server.server_id, toolName: selected.name },
                isError: result?.isError === true,
              };
            },
          });
          activeToolNames.push(selected.pi_tool_name);
        }
      }
      ctx.ui.setStatus(
        "factory-mcp",
        JSON.stringify({ state: "ready", tools: activeToolNames }),
      );
    } catch {
      for (const client of clients) client.close();
      ctx.ui.setStatus("factory-mcp", JSON.stringify({ state: "error" }));
      throw new Error("Factory-approved MCP bridge failed to initialize");
    }
  });
  pi.on("session_shutdown", async () => {
    for (const client of clients) client.close();
    clients.length = 0;
  });
}
'''


def prepare_mcp_bridge(
    resolved: ResolvedSpecialistProfile,
    directory: Path,
) -> MCPBridgeFiles:
    """Write the bridge extension and a private profile-scoped MCP manifest."""
    if not resolved.mcp_servers:
        raise MCPBridgeError("profile does not select any MCP tools")
    try:
        directory.mkdir(parents=True, exist_ok=True, mode=0o700)
        extension_path = directory / "factory-mcp-bridge.ts"
        config_path = directory / "factory-mcp-config.json"
        manifest = profile_resource_manifest(resolved)
        config_path.write_text(
            json.dumps(manifest, separators=(",", ":")),
            encoding="utf-8",
        )
        config_path.chmod(0o600)
        extension_path.write_text(PI_MCP_BRIDGE_EXTENSION, encoding="utf-8")
        extension_path.chmod(0o600)
    except OSError as error:
        raise MCPBridgeError("could not prepare the MCP bridge resources") from error

    tool_names = tuple(
        tool["pi_tool_name"]
        for server in manifest["servers"]
        for tool in server["tools"]
    )
    return MCPBridgeFiles(
        extension_path=extension_path,
        config_path=config_path,
        tool_names=tool_names,
    )
