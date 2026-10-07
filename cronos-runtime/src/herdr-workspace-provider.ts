import { lstat, mkdir, realpath, rm } from "node:fs/promises";
import { isAbsolute, join, relative, resolve, sep } from "node:path";
import type {
  AgentPane,
  AgentRole,
  CreateWorkspaceInput,
  TaskWorkspace,
  WorkspaceProvider,
} from "./contracts";

type HerdrWorkspaceResponse = {
  result?: {
    workspace?: { workspace_id?: string };
    root_pane?: { pane_id?: string };
  };
};

type HerdrSplitResponse = {
  result?: { pane?: { pane_id?: string } };
};

type WorkspaceState = {
  handle: TaskWorkspace;
  repositoryPath: string;
  worktreePath: string;
  containerName: string;
  herdrWorkspaceId: string;
  rootPaneId: string;
  panes: Set<string>;
};

export type HerdrWorkspaceProviderOptions = {
  runtimeRoot: string;
  runtimeImage: string;
  herdrBinary?: string;
  dockerBinary?: string;
  gitBinary?: string;
  runtimeEnv?: readonly string[];
};

async function run(command: string, args: string[], cwd?: string): Promise<string> {
  const process = Bun.spawn({
    cmd: [command, ...args],
    cwd,
    stdout: "pipe",
    stderr: "pipe",
  });
  const [stdout, stderr, exitCode] = await Promise.all([
    new Response(process.stdout).text(),
    new Response(process.stderr).text(),
    process.exited,
  ]);

  if (exitCode !== 0) {
    throw new Error(`${command} ${args.join(" ")} failed (${exitCode}): ${stderr.trim()}`);
  }
  return stdout.trim();
}

function isWithin(root: string, path: string): boolean {
  const rel = relative(root, path);
  return rel === "" || (rel !== ".." && !rel.startsWith(`..${sep}`) && !isAbsolute(rel));
}

function readResult<T>(output: string, select: (value: any) => T | undefined, context: string): T {
  let parsed: unknown;
  try {
    parsed = JSON.parse(output);
  } catch {
    throw new Error(`Herdr returned invalid JSON while ${context}`);
  }

  const result = select(parsed);
  if (result === undefined || result === null || result === "") {
    throw new Error(`Herdr response omitted required identifiers while ${context}`);
  }
  return result;
}

async function launchRuntime(
  options: HerdrWorkspaceProviderOptions,
  workspace: TaskWorkspace,
  repositoryPath: string,
  commonGitDir: string,
  worktreeGitDir: string,
): Promise<WorkspaceState> {
  const herdrBinary = options.herdrBinary ?? "herdr";
  const dockerBinary = options.dockerBinary ?? "docker";
  const containerName = `cronos-${workspace.id.replaceAll("-", "")}`;
  let containerCreated = false;
  let herdrWorkspaceId: string | undefined;

  try {
    const dockerArgs = [
      "run",
      "--detach",
      "--name", containerName,
      "--label", `cronos.task=${workspace.taskId}`,
      "--label", `cronos.workspace=${workspace.id}`,
      "--network", "bridge",
      "--cap-drop", "ALL",
      "--security-opt", "no-new-privileges",
      "--mount", `type=bind,source=${workspace.rootPath},target=/workspace`,
      "--mount", `type=bind,source=${commonGitDir},target=${commonGitDir},readonly`,
      "--mount", `type=bind,source=${worktreeGitDir},target=${worktreeGitDir}`,
      "--workdir", "/workspace",
    ];
    for (const name of options.runtimeEnv ?? []) dockerArgs.push("--env", name);
    dockerArgs.push(options.runtimeImage, "sh", "-c", "while true; do sleep 3600; done");
    await run(dockerBinary, dockerArgs);
    containerCreated = true;

    const workspaceOutput = await run(herdrBinary, [
      "workspace", "create",
      "--cwd", workspace.rootPath,
      "--label", `cronos-${workspace.id}`,
      "--no-focus",
    ]);
    herdrWorkspaceId = readResult(
      workspaceOutput,
      (value) => (value as HerdrWorkspaceResponse).result?.workspace?.workspace_id,
      "creating workspace",
    );
    const rootPaneId = readResult(
      workspaceOutput,
      (value) => (value as HerdrWorkspaceResponse).result?.root_pane?.pane_id,
      "creating workspace",
    );

    return {
      handle: workspace,
      repositoryPath,
      worktreePath: workspace.rootPath,
      containerName,
      herdrWorkspaceId,
      rootPaneId,
      panes: new Set(),
    };
  } catch (error) {
    if (herdrWorkspaceId) await run(herdrBinary, ["workspace", "close", herdrWorkspaceId]).catch(() => {});
    if (containerCreated) await run(dockerBinary, ["rm", "--force", containerName]).catch(() => {});
    throw error;
  }
}

export class HerdrWorkspaceProvider implements WorkspaceProvider {
  private readonly workspaces = new Map<string, WorkspaceState>();
  private readonly runtimeRoot: string;
  private readonly herdrBinary: string;
  private readonly dockerBinary: string;
  private readonly gitBinary: string;
  private readonly runtimeEnv: readonly string[];

  constructor(private readonly options: HerdrWorkspaceProviderOptions) {
    if (!options.runtimeImage.trim()) throw new Error("runtimeImage is required");
    this.runtimeRoot = resolve(options.runtimeRoot);
    this.herdrBinary = options.herdrBinary ?? "herdr";
    this.dockerBinary = options.dockerBinary ?? "docker";
    this.gitBinary = options.gitBinary ?? "git";
    this.runtimeEnv = [...new Set(options.runtimeEnv ?? [])];
    for (const name of this.runtimeEnv) {
      if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(name)) {
        throw new Error(`Invalid runtime environment variable name: ${name}`);
      }
    }
  }

  async createWorkspace(input: CreateWorkspaceInput): Promise<TaskWorkspace> {
    const repositoryPath = await realpath(input.repositoryPath);
    const repositoryRoot = await run(this.gitBinary, ["-C", repositoryPath, "rev-parse", "--show-toplevel"]);
    const commonGitDirValue = await run(this.gitBinary, [
      "-C", repositoryRoot, "rev-parse", "--path-format=absolute", "--git-common-dir",
    ]);
    const commonGitDir = await realpath(resolve(repositoryRoot, commonGitDirValue));
    await run(this.gitBinary, [
      "-C", repositoryRoot, "rev-parse", "--verify", `${input.baseBranch}^{commit}`,
    ]);
    await mkdir(this.runtimeRoot, { recursive: true });
    const runtimeRoot = await realpath(this.runtimeRoot);

    const id = crypto.randomUUID();
    const taskWorkspace: TaskWorkspace = {
      id,
      taskId: input.taskId,
      rootPath: join(runtimeRoot, `worktree-${id}`),
    };
    let worktreeCreated = false;

    try {
      await run(this.gitBinary, [
        "-C", repositoryRoot, "worktree", "add", "--detach", taskWorkspace.rootPath, input.baseBranch,
      ]);
      worktreeCreated = true;
      const worktreeGitDirValue = await run(this.gitBinary, [
        "-C", taskWorkspace.rootPath, "rev-parse", "--path-format=absolute", "--git-dir",
      ]);
      const worktreeGitDir = await realpath(worktreeGitDirValue);
      if (!isWithin(commonGitDir, worktreeGitDir) || worktreeGitDir === commonGitDir) {
        throw new Error("Task worktree Git metadata is not isolated beneath the repository common directory");
      }

      const state = await launchRuntime(this.options, taskWorkspace, repositoryRoot, commonGitDir, worktreeGitDir);
      this.workspaces.set(id, state);
      return taskWorkspace;
    } catch (error) {
      if (worktreeCreated) {
        await run(this.gitBinary, [
          "-C", repositoryRoot, "worktree", "remove", "--force", taskWorkspace.rootPath,
        ]).catch(() => {});
        await rm(taskWorkspace.rootPath, { recursive: true, force: true }).catch(() => {});
      }
      throw error;
    }
  }

  async restoreWorkspace(workspace: TaskWorkspace, repositoryPath: string): Promise<TaskWorkspace> {
    if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(workspace.id)) {
      throw new Error("Invalid task workspace identifier");
    }
    const rootPath = await this.validateWorkspacePath(workspace);
    const realRepositoryPath = await realpath(repositoryPath);
    const repositoryRoot = await run(this.gitBinary, ["-C", realRepositoryPath, "rev-parse", "--show-toplevel"]);
    const commonGitDir = await realpath(await run(this.gitBinary, [
      "-C", repositoryRoot, "rev-parse", "--path-format=absolute", "--git-common-dir",
    ]));
    const worktreeCommonGitDir = await realpath(await run(this.gitBinary, [
      "-C", rootPath, "rev-parse", "--path-format=absolute", "--git-common-dir",
    ]));
    const worktreeGitDir = await realpath(await run(this.gitBinary, [
      "-C", rootPath, "rev-parse", "--path-format=absolute", "--git-dir",
    ]));
    if (worktreeCommonGitDir !== commonGitDir || !isWithin(commonGitDir, worktreeGitDir) || worktreeGitDir === commonGitDir) {
      throw new Error("Persisted task worktree does not belong to the configured repository");
    }

    const restoredWorkspace = { ...workspace, rootPath };
    await this.suspendWorkspace(restoredWorkspace);
    const state = await launchRuntime(this.options, restoredWorkspace, repositoryRoot, commonGitDir, worktreeGitDir);
    this.workspaces.set(workspace.id, state);
    return restoredWorkspace;
  }

  async suspendWorkspace(workspace: TaskWorkspace): Promise<void> {
    if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(workspace.id)) {
      throw new Error("Invalid task workspace identifier");
    }
    const state = this.workspaces.get(workspace.id);
    if (state && (
      workspace.taskId !== state.handle.taskId ||
      resolve(workspace.rootPath) !== state.worktreePath
    )) {
      throw new Error(`Task workspace identity does not match ${workspace.id}`);
    }
    const ownedWorkspace = state?.handle ?? workspace;
    this.workspaces.delete(workspace.id);
    const failures: unknown[] = [];

    let herdrWorkspaceId = state?.herdrWorkspaceId;
    if (!herdrWorkspaceId) {
      try {
        herdrWorkspaceId = await this.findHerdrWorkspaceId(`cronos-${ownedWorkspace.id}`);
      } catch (error) {
        failures.push(error);
      }
    }
    if (herdrWorkspaceId) {
      try {
        await run(this.herdrBinary, ["workspace", "close", herdrWorkspaceId]);
      } catch (error) {
        failures.push(error);
      }
    }
    try {
      await this.removeContainerIfOwned(ownedWorkspace);
    } catch (error) {
      failures.push(error);
    }
    if (failures.length > 0) throw new AggregateError(failures, `Could not suspend task workspace ${workspace.id}`);
  }

  private async validateWorkspacePath(workspace: TaskWorkspace): Promise<string> {
    const runtimeRoot = await realpath(this.runtimeRoot);
    const expectedPath = join(runtimeRoot, `worktree-${workspace.id}`);
    if (resolve(workspace.rootPath) !== expectedPath) {
      throw new Error("Persisted task workspace path is outside its runtime-owned directory");
    }
    const details = await lstat(expectedPath);
    if (!details.isDirectory() || details.isSymbolicLink()) {
      throw new Error("Persisted task workspace is not a regular directory");
    }
    const actualPath = await realpath(expectedPath);
    if (actualPath !== expectedPath) throw new Error("Persisted task workspace resolves outside its runtime-owned directory");
    return actualPath;
  }

  private async findHerdrWorkspaceId(label: string): Promise<string | undefined> {
    const output = await run(this.herdrBinary, ["workspace", "list"]);
    const value = JSON.parse(output) as {
      result?: { workspaces?: Array<{ workspace_id?: string; label?: string }> };
    };
    const workspaces = value.result?.workspaces;
    if (!Array.isArray(workspaces)) throw new Error("Herdr workspace list response was invalid");
    const matches = workspaces.filter((item) => item.label === label);
    if (matches.length > 1) throw new Error(`Multiple Herdr workspaces use the task label ${label}`);
    const id = matches[0]?.workspace_id;
    if (id !== undefined && typeof id !== "string") throw new Error("Herdr workspace identifier was invalid");
    return id;
  }

  private async removeContainerIfOwned(workspace: TaskWorkspace): Promise<void> {
    const containerName = `cronos-${workspace.id.replaceAll("-", "")}`;
    let output: string;
    try {
      output = await run(this.dockerBinary, ["inspect", containerName]);
    } catch (error) {
      if (error instanceof Error && /No such (object|container)/i.test(error.message)) return;
      throw error;
    }
    const inspected = JSON.parse(output) as Array<{
      Config?: { Labels?: Record<string, string> };
    }>;
    const labels = inspected[0]?.Config?.Labels;
    if (labels?.["cronos.workspace"] !== workspace.id || labels?.["cronos.task"] !== workspace.taskId) {
      throw new Error(`Refusing to remove unowned Docker container ${containerName}`);
    }
    await run(this.dockerBinary, ["rm", "--force", containerName]);
  }

  async openPane(workspace: TaskWorkspace, role: AgentRole): Promise<AgentPane> {
    const state = this.workspaces.get(workspace.id);
    if (!state) throw new Error(`Unknown task workspace: ${workspace.id}`);

    const output = await run(this.herdrBinary, [
      "pane", "split", state.rootPaneId,
      "--direction", "right",
      "--cwd", state.worktreePath,
      "--no-focus",
    ]);
    const paneId = readResult(
      output,
      (value) => (value as HerdrSplitResponse).result?.pane?.pane_id,
      "creating an agent pane",
    );

    try {
      await run(this.herdrBinary, ["pane", "rename", paneId, role]);
      await run(this.herdrBinary, [
        "pane", "run", paneId, `docker exec -it ${state.containerName} sh`,
      ]);
      state.panes.add(paneId);
      return { id: paneId, workspaceId: workspace.id };
    } catch (error) {
      await run(this.herdrBinary, ["pane", "close", paneId]).catch(() => {});
      throw error;
    }
  }

  async closePane(pane: AgentPane): Promise<void> {
    const state = this.workspaces.get(pane.workspaceId);
    if (!state || !state.panes.has(pane.id)) {
      throw new Error(`Unknown agent pane: ${pane.id}`);
    }
    await run(this.herdrBinary, ["pane", "close", pane.id]);
    state.panes.delete(pane.id);
  }

  async closeWorkspace(workspace: TaskWorkspace): Promise<void> {
    const state = this.workspaces.get(workspace.id);
    if (!state) throw new Error(`Unknown task workspace: ${workspace.id}`);
    if (workspace.taskId !== state.handle.taskId || resolve(workspace.rootPath) !== state.worktreePath) {
      throw new Error(`Task workspace identity does not match ${workspace.id}`);
    }
    this.workspaces.delete(workspace.id);

    const failures: unknown[] = [];
    try {
      await run(this.herdrBinary, ["workspace", "close", state.herdrWorkspaceId]);
    } catch (error) {
      failures.push(error);
    }

    let containerStopped = false;
    try {
      await this.removeContainerIfOwned(state.handle);
      containerStopped = true;
    } catch (error) {
      failures.push(error);
    }

    if (containerStopped) {
      try {
        const safeWorktreePath = await this.validateWorkspacePath(state.handle);
        await run(this.gitBinary, [
          "-C", state.repositoryPath, "worktree", "remove", "--force", safeWorktreePath,
        ]);
        await rm(safeWorktreePath, { recursive: true, force: true });
      } catch (error) {
        failures.push(error);
      }
    }

    if (failures.length) {
      throw new AggregateError(failures, `Failed to fully clean task workspace ${workspace.id}`);
    }
  }
}
