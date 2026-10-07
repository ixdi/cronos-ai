import { lstat, mkdir, realpath, rm } from "node:fs/promises";
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import type { AgentRole, AgentRunner, AgentRunEvent, AgentRunInput, AgentRunResult } from "./contracts";

const TRIAGE_SYSTEM_PROMPT =
  "For task routing, use Pi codemode to call the TypeSafe Jev classifier (provider typesafe, model jev-latest) with models.classify(). Report its classification; if Jev is unavailable, report that instead of silently substituting your own classification.";
const MAX_EVENT_BYTES = 50 * 1024 * 1024;

export type PiCliAgentRunnerOptions = {
  chatModel: string;
  herdrBinary?: string;
  piBinary?: string;
  timeoutMs?: number;
  pollIntervalMs?: number;
};

type PiEvent = {
  type?: string;
  message?: { role?: string; content?: Array<{ type?: string; text?: string }>; stopReason?: string };
  toolName?: string;
  assistantMessageEvent?: { type?: string; delta?: string };
  errorMessage?: string;
  finalError?: string;
};

function shellQuote(value: string): string {
  return `'${value.replaceAll("'", "'\\''")}'`;
}

function piToolsForRole(role: AgentRole): string {
  if (role === "triage") return "read,codemode";
  if (role === "code-review" || role === "verification") return "read,bash";
  return "read,bash,edit,write";
}

async function runCommand(command: string, args: string[], cwd?: string): Promise<string> {
  const process = Bun.spawn({ cmd: [command, ...args], cwd, stdout: "pipe", stderr: "pipe" });
  const [stdout, stderr, exitCode] = await Promise.all([
    new Response(process.stdout).text(),
    new Response(process.stderr).text(),
    process.exited,
  ]);
  if (exitCode !== 0) {
    throw new Error(`${command} failed (${exitCode}): ${stderr.trim() || "no diagnostic output"}`);
  }
  return stdout;
}

function isWithin(root: string, path: string): boolean {
  const rel = relative(root, path);
  return rel === "" || (rel !== ".." && !rel.startsWith(`..${sep}`) && !isAbsolute(rel));
}

async function prepareAgentOutputDirectory(workspaceRoot: string): Promise<string> {
  const [commonGitDirValue, gitPathValue] = await Promise.all([
    runCommand("git", ["-C", workspaceRoot, "rev-parse", "--path-format=absolute", "--git-common-dir"]),
    runCommand("git", ["-C", workspaceRoot, "rev-parse", "--path-format=absolute", "--git-path", "cronos-agent-output"]),
  ]);
  const commonGitDir = await realpath(commonGitDirValue.trim());
  const worktreeGitDir = await realpath(dirname(gitPathValue.trim()));
  if (!isWithin(commonGitDir, worktreeGitDir)) throw new Error("Git metadata path escaped the repository");

  const outputDirectory = join(worktreeGitDir, basename(gitPathValue.trim()));
  try {
    await mkdir(outputDirectory);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "EEXIST") throw error;
  }
  const info = await lstat(outputDirectory);
  if (!info.isDirectory() || info.isSymbolicLink()) throw new Error("Agent output directory is not a real directory");
  const actualPath = await realpath(outputDirectory);
  if (!isWithin(commonGitDir, actualPath)) throw new Error("Agent output directory escaped Git metadata");
  return actualPath;
}

async function ownedFileText(path: string, allowedRoot: string): Promise<string | undefined> {
  if (resolve(path) !== path || dirname(path) !== allowedRoot) {
    throw new Error("Agent output path escaped its task workspace");
  }
  let info;
  try {
    info = await lstat(path);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return undefined;
    throw error;
  }
  if (!info.isFile() || info.isSymbolicLink()) {
    throw new Error("Pi output is not a regular file in the task workspace");
  }
  if (info.size > MAX_EVENT_BYTES) throw new Error("Pi event output exceeded the configured size limit");
  return Bun.file(path).text();
}

function extractAssistantText(message: PiEvent["message"]): string {
  return message?.content?.filter((block) => block.type === "text").map((block) => block.text ?? "").join("").trim() ?? "";
}

async function changedFiles(workspaceRoot: string): Promise<string[]> {
  const [tracked, untracked] = await Promise.all([
    runCommand("git", ["-C", workspaceRoot, "diff", "HEAD", "--name-only", "-z"]),
    runCommand("git", ["-C", workspaceRoot, "ls-files", "--others", "--exclude-standard", "-z"]),
  ]);
  return [...new Set([...tracked.split("\0"), ...untracked.split("\0")].filter(Boolean))].sort();
}

export class PiCliAgentRunner implements AgentRunner {
  private readonly herdrBinary: string;
  private readonly piBinary: string;
  private readonly timeoutMs: number;
  private readonly pollIntervalMs: number;
  private readonly chatModel: string;

  constructor(private readonly options: PiCliAgentRunnerOptions) {
    if (!options.chatModel.trim()) throw new Error("A Pi chat model must be configured");
    this.chatModel = options.chatModel.trim();
    this.herdrBinary = options.herdrBinary ?? "herdr";
    this.piBinary = options.piBinary ?? "pi";
    this.timeoutMs = options.timeoutMs ?? 30 * 60_000;
    this.pollIntervalMs = options.pollIntervalMs ?? 150;
    if (!Number.isFinite(this.timeoutMs) || this.timeoutMs <= 0) throw new Error("timeoutMs must be positive");
    if (!Number.isFinite(this.pollIntervalMs) || this.pollIntervalMs <= 0) throw new Error("pollIntervalMs must be positive");
  }

  async run(input: AgentRunInput): Promise<AgentRunResult> {
    if (input.signal.aborted) throw new DOMException("Agent run was cancelled", "AbortError");

    const workspaceRoot = await realpath(input.workspace.rootPath);
    const outputDirectory = await prepareAgentOutputDirectory(workspaceRoot);
    const runId = crypto.randomUUID().replaceAll("-", "");
    const eventsName = `.cronos-agent-${runId}.jsonl`;
    const statusName = `.cronos-agent-${runId}.status`;
    const eventsPath = join(outputDirectory, eventsName);
    const statusPath = join(outputDirectory, statusName);
    const triage = input.role === "triage";
    const args = [
      this.piBinary,
      "--mode", "json",
      "--no-session",
      "--model", this.chatModel,
      "--tools", piToolsForRole(input.role),
    ];
    if (triage) args.push("--append-system-prompt", TRIAGE_SYSTEM_PROMPT);
    args.push("--", input.prompt);

    const command = `${args.map(shellQuote).join(" ")} > ${shellQuote(eventsPath)} 2>/dev/null; code=$?; printf '%s\\n' "$code" > ${shellQuote(statusPath)}`;
    let finished = false;
    let commandStarted = false;
    let stopRequested = false;
    let eventOffset = 0;
    let partialLine = "";
    let assistantSummary = "";
    let assistantError: string | undefined;
    let settled = false;
    const emit = (event: AgentRunEvent) => input.onEvent(event);

    const stopPi = async () => {
      if (stopRequested) return;
      stopRequested = true;
      if (!commandStarted) return;
      await runCommand(this.herdrBinary, ["pane", "send-keys", input.pane.id, "ctrl+c"], workspaceRoot).catch(() => {});
      const cancellationDeadline = Date.now() + 2_000;
      while (Date.now() < cancellationDeadline) {
        const status = await ownedFileText(statusPath, outputDirectory).catch(() => undefined);
        if (status !== undefined) return;
        await Bun.sleep(25);
      }
    };

    const consumeEvents = async (flush: boolean) => {
      const contents = await ownedFileText(eventsPath, outputDirectory);
      if (contents === undefined) return;
      const appended = contents.slice(eventOffset);
      eventOffset = contents.length;
      const lines = (partialLine + appended).split("\n");
      partialLine = flush ? "" : lines.pop() ?? "";
      if (flush && lines.at(-1) === "") lines.pop();
      for (const line of lines) {
        if (!line.trim()) continue;
        let event: PiEvent;
        try {
          event = JSON.parse(line) as PiEvent;
        } catch {
          throw new Error("Pi emitted a malformed JSONL event");
        }
        if (event.type === "agent_settled") settled = true;
        if (event.type === "message_end" && event.message?.role === "assistant") {
          const text = extractAssistantText(event.message);
          if (text) assistantSummary = text;
          assistantError = ["error", "aborted"].includes(event.message.stopReason ?? "")
            ? event.message.stopReason
            : undefined;
        }
        if (event.type === "tool_execution_start") {
          emit({ type: "progress", message: `Pi started tool: ${event.toolName ?? "unknown"}` });
        }
        if (event.type === "auto_retry_start") {
          emit({ type: "progress", message: "Pi is retrying a model request" });
        }
        if (event.type === "agent_end" && event.errorMessage) assistantError = event.errorMessage;
        if (event.type === "auto_retry_end" && event.finalError) assistantError = event.finalError;
      }
    };

    try {
      emit({ type: "started", message: `Starting Pi ${input.role} run` });
      await runCommand(this.herdrBinary, ["pane", "run", input.pane.id, command], workspaceRoot);
      commandStarted = true;

      const deadline = Date.now() + this.timeoutMs;
      while (true) {
        if (input.signal.aborted) {
          await stopPi();
          throw new DOMException("Agent run was cancelled", "AbortError");
        }
        await consumeEvents(false);
        const statusText = await ownedFileText(statusPath, outputDirectory);
        if (statusText !== undefined) {
          finished = true;
          await consumeEvents(true);
          const exitCode = Number(statusText.trim());
          if (!Number.isInteger(exitCode)) throw new Error("Pi wrote an invalid process exit status");
          if (exitCode !== 0 || assistantError) {
            throw new Error(`Pi run failed${assistantError ? ` (${assistantError})` : ` with exit code ${exitCode}`}`);
          }
          if (!settled) throw new Error("Pi exited without an agent_settled event");
          if (!assistantSummary) throw new Error("Pi completed without a final assistant response");
          const files = await changedFiles(workspaceRoot);
          emit({ type: "completed", message: assistantSummary });
          return { summary: assistantSummary, changedFiles: files };
        }
        if (Date.now() >= deadline) {
          await stopPi();
          throw new Error(`Pi run exceeded its ${this.timeoutMs}ms timeout`);
        }
        await Bun.sleep(this.pollIntervalMs);
      }
    } catch (error) {
      if (!(error instanceof DOMException && error.name === "AbortError")) {
        emit({ type: "failed", message: "Pi agent run failed" });
      }
      throw error;
    } finally {
      if (!finished) await stopPi();
      for (const path of [eventsPath, statusPath]) {
        try {
          const info = await lstat(path);
          if (info.isFile() && !info.isSymbolicLink()) await rm(path, { force: true });
        } catch (error) {
          if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
        }
      }
    }
  }
}
