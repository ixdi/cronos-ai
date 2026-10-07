import type { Database } from "bun:sqlite";
import { resumeInterruptedTask, resolveTaskReview } from "cronos-core/task-workflow";
import type { TaskWorkflowDependencies, TaskWorkflowResult } from "cronos-core/task-workflow";
import { GitHubAppTokenProvider, githubAppTokenConfigFromEnvironment } from "cronos-core/github-app-token";
import { GitHubPullRequestService } from "cronos-core/github-delivery";
import { HerdrWorkspaceProvider } from "cronos-runtime/herdr-workspace-provider";
import { PiCliAgentRunner } from "cronos-runtime/pi-cli-agent-runner";
import { openStorageDatabase } from "cronos-storage/database";
import { inspectTask, listTaskSummaries } from "cronos-storage/task-inspection";
import type { TaskStatus } from "cronos-storage/workflow-store";

const taskStatuses = new Set<TaskStatus>([
  "queued",
  "active",
  "blocked",
  "errored",
  "interrupted",
  "review_pending",
  "delivered",
]);

function safeErrorText(value: string): string {
  return value.replace(/[\u0000-\u001f\u007f-\u009f]/g, (character) =>
    `\\u${character.codePointAt(0)!.toString(16).padStart(4, "0")}`);
}

const usage = `Cronos local task inspection

Usage:
  cronos-panel [--db PATH] list [--status STATUS]
  cronos-panel [--db PATH] show TASK_ID
  cronos-panel [--db PATH] resume TASK_ID
  cronos-panel [--db PATH] approve TASK_ID
  cronos-panel [--db PATH] reject TASK_ID --feedback TEXT
  cronos-panel --help

Statuses: queued, active, blocked, errored, interrupted, review_pending, delivered
Database path defaults to CRONOS_DB_PATH or ./cronos.sqlite.
Workflow actions require CRONOS_REPOSITORY_PATH, CRONOS_RUNTIME_ROOT, CRONOS_RUNTIME_IMAGE, and CRONOS_PI_CHAT_MODEL. Approval and resume also require the GitHub App settings.
Output is JSON; task details include the latest code-review and verification results.`;

type ParsedCommand =
  | { kind: "help" }
  | { kind: "list"; databasePath: string; status?: TaskStatus }
  | { kind: "show"; databasePath: string; taskId: string }
  | { kind: "resume"; databasePath: string; taskId: string }
  | { kind: "approve"; databasePath: string; taskId: string }
  | { kind: "reject"; databasePath: string; taskId: string; feedback: string };

export type WorkflowAction = "resume" | "approve" | "reject";

export type WorkflowDependenciesFactory = (
  db: Database,
  env: Record<string, string | undefined>,
  action: WorkflowAction,
) => Omit<TaskWorkflowDependencies, "db">;

function requiredSetting(env: Record<string, string | undefined>, name: string): string {
  const value = env[name]?.trim();
  if (!value) throw new Error(`Missing required environment variable ${name}`);
  return value;
}

export const createDefaultWorkflowDependencies: WorkflowDependenciesFactory = (_db, env, action) => {
  const runtimeEnv = (env.CRONOS_RUNTIME_ENV ?? "").split(",").map((name) => name.trim()).filter(Boolean);
  const githubCredentialNames = new Set([
    "CRONOS_GITHUB_APP_PRIVATE_KEY",
    "CRONOS_GITHUB_INSTALLATION_TOKEN",
    "GITHUB_TOKEN",
    "GH_TOKEN",
  ]);
  if (runtimeEnv.some((name) => githubCredentialNames.has(name))) {
    throw new Error("GitHub App credentials cannot be forwarded to task containers");
  }
  const herdrBinary = env.HERDR_BINARY || undefined;
  const repositoryPath = requiredSetting(env, "CRONOS_REPOSITORY_PATH");
  const runtimeRoot = requiredSetting(env, "CRONOS_RUNTIME_ROOT");
  const baseBranch = env.CRONOS_BASE_BRANCH?.trim() || "main";
  const dependencies: Omit<TaskWorkflowDependencies, "db"> = {
    repositoryPath,
    baseBranch,
    workspaceProvider: new HerdrWorkspaceProvider({
      runtimeRoot,
      runtimeImage: requiredSetting(env, "CRONOS_RUNTIME_IMAGE"),
      runtimeEnv,
      herdrBinary,
      dockerBinary: env.DOCKER_BINARY || undefined,
      gitBinary: env.GIT_BINARY || undefined,
    }),
    agentRunner: new PiCliAgentRunner({
      chatModel: requiredSetting(env, "CRONOS_PI_CHAT_MODEL"),
      herdrBinary,
      piBinary: env.PI_BINARY || undefined,
    }),
  };
  if (action !== "reject") {
    dependencies.pullRequestCreator = new GitHubPullRequestService({
      repositoryPath,
      runtimeRoot,
      owner: requiredSetting(env, "CRONOS_GITHUB_OWNER"),
      repository: requiredSetting(env, "CRONOS_GITHUB_REPOSITORY"),
      baseBranch,
      tokenProvider: new GitHubAppTokenProvider(githubAppTokenConfigFromEnvironment(env)),
      gitBinary: env.GIT_BINARY || undefined,
    });
  }
  return dependencies;
};

export type CliResult = {
  exitCode: number;
  output?: string;
  error?: string;
};

function parseCommand(args: string[], env: Record<string, string | undefined>): ParsedCommand | CliResult {
  if (args.length === 0 || args[0] === "--help" || args[0] === "help") return { kind: "help" };

  let index = 0;
  let databasePath = env.CRONOS_DB_PATH || "cronos.sqlite";
  if (args[index] === "--db") {
    const value = args[index + 1];
    if (!value || value.startsWith("--")) return { exitCode: 2, error: "--db requires a database path" };
    databasePath = value;
    index += 2;
  }

  const command = args[index++];
  if (command === "list") {
    let status: TaskStatus | undefined;
    if (index < args.length) {
      if (args[index] !== "--status" || !args[index + 1] || index + 2 !== args.length) {
        return { exitCode: 2, error: "list accepts only one --status STATUS option" };
      }
      const value = args[index + 1] as TaskStatus;
      if (!taskStatuses.has(value)) {
        return { exitCode: 2, error: `Unknown task status: ${safeErrorText(value)}` };
      }
      status = value;
    }
    return { kind: "list", databasePath, status };
  }

  if (command === "show" || command === "resume" || command === "approve") {
    const taskId = args[index];
    if (!taskId || index + 1 !== args.length) {
      return { exitCode: 2, error: `${command} requires exactly one task ID` };
    }
    return { kind: command, databasePath, taskId };
  }

  if (command === "reject") {
    const taskId = args[index];
    const feedbackFlag = args[index + 1];
    const feedback = args[index + 2];
    if (!taskId || feedbackFlag !== "--feedback" || !feedback?.trim() || index + 3 !== args.length) {
      return { exitCode: 2, error: "reject requires TASK_ID --feedback TEXT" };
    }
    return { kind: "reject", databasePath, taskId, feedback };
  }

  return { exitCode: 2, error: "Expected list, show, resume, approve, reject, or --help" };
}

export async function executePanelCli(
  args: string[],
  env: Record<string, string | undefined> = process.env,
  workflowDependenciesFactory: WorkflowDependenciesFactory = createDefaultWorkflowDependencies,
): Promise<CliResult> {
  const command = parseCommand(args, env);
  if ("exitCode" in command) return command;
  if (command.kind === "help") return { exitCode: 0, output: usage };

  let db: Awaited<ReturnType<typeof openStorageDatabase>> | undefined;
  try {
    db = await openStorageDatabase(command.databasePath);
    if (command.kind === "list") {
      return {
        exitCode: 0,
        output: JSON.stringify(listTaskSummaries(db, command.status), null, 2),
      };
    }
    if (command.kind === "show") {
      const task = inspectTask(db, command.taskId);
      if (!task) return { exitCode: 1, error: `Task not found: ${safeErrorText(command.taskId)}` };
      return { exitCode: 0, output: JSON.stringify(task, null, 2) };
    }

    const dependencies: TaskWorkflowDependencies = {
      ...workflowDependenciesFactory(db, env, command.kind),
      db,
    };
    let actionResult: TaskWorkflowResult;
    if (command.kind === "resume") {
      actionResult = await resumeInterruptedTask(dependencies, command.taskId);
    } else {
      actionResult = await resolveTaskReview(dependencies, command.taskId, {
        approved: command.kind === "approve",
        feedback: command.kind === "reject" ? command.feedback : "",
      });
    }
    const output = { ...actionResult };
    delete (output as { workspace?: unknown }).workspace;
    return {
      exitCode: actionResult.status === "errored" || actionResult.status === "resume_failed" ? 1 : 0,
      output: JSON.stringify(output, null, 2),
    };
  } catch (error) {
    const message = error instanceof Error ? error.message : "Unknown error";
    const operation = command.kind === "list" || command.kind === "show" ? "inspection" : "workflow action";
    return { exitCode: 1, error: `Cronos ${operation} failed: ${safeErrorText(message)}` };
  } finally {
    db?.close(true);
  }
}

export async function main(args: string[] = process.argv.slice(2)): Promise<void> {
  const result = await executePanelCli(args);
  if (result.output !== undefined) console.log(result.output);
  if (result.error !== undefined) console.error(result.error);
  process.exitCode = result.exitCode;
}

if (import.meta.main) await main();
