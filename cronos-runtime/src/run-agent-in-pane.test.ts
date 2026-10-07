import { expect, test } from "bun:test";
import type {
  AgentPane,
  AgentRole,
  AgentRunner,
  AgentRunEvent,
  AgentRunResult,
  CreateWorkspaceInput,
  TaskWorkspace,
  WorkspaceProvider,
} from "./contracts";
import { runAgentInPane } from "./run-agent-in-pane";

class FakeWorkspaceProvider implements WorkspaceProvider {
  readonly calls: string[] = [];

  async createWorkspace(input: CreateWorkspaceInput): Promise<TaskWorkspace> {
    this.calls.push("create-workspace");
    return { id: "workspace-1", taskId: input.taskId, rootPath: "/tmp/task-1" };
  }

  async restoreWorkspace(workspace: TaskWorkspace): Promise<TaskWorkspace> {
    this.calls.push(`restore-workspace:${workspace.id}`);
    return workspace;
  }

  async suspendWorkspace(workspace: TaskWorkspace): Promise<void> {
    this.calls.push(`suspend-workspace:${workspace.id}`);
  }

  async openPane(workspace: TaskWorkspace, _role: AgentRole): Promise<AgentPane> {
    this.calls.push(`open-pane:${workspace.id}`);
    return { id: "pane-1", workspaceId: workspace.id };
  }

  async closePane(pane: AgentPane): Promise<void> {
    this.calls.push(`close-pane:${pane.id}`);
  }

  async closeWorkspace(workspace: TaskWorkspace): Promise<void> {
    this.calls.push(`close-workspace:${workspace.id}`);
  }
}

test("creates a pane, reports agent progress/results, and closes resources", async () => {
  const provider = new FakeWorkspaceProvider();
  const workspace = await provider.createWorkspace({
    taskId: "task-1",
    repositoryPath: "/repo",
    baseBranch: "main",
  });
  const events: AgentRunEvent[] = [];
  const runner: AgentRunner = {
    async run(input) {
      provider.calls.push(`run:${input.role}:${input.pane.id}`);
      input.onEvent({ type: "started", message: "Started" });
      input.onEvent({ type: "progress", message: "Changed one file" });
      return { summary: "Implemented change", changedFiles: ["src/example.ts"] };
    },
  };

  const result = await runAgentInPane(provider, runner, {
    taskId: "task-1",
    role: "implementation",
    prompt: "Implement the task",
    workspace,
    signal: new AbortController().signal,
    onEvent: (event) => events.push(event),
  });
  await provider.closeWorkspace(workspace);

  expect(result).toEqual({ summary: "Implemented change", changedFiles: ["src/example.ts"] });
  expect(events.map(({ type }) => type)).toEqual(["started", "progress"]);
  expect(provider.calls).toEqual([
    "create-workspace",
    "open-pane:workspace-1",
    "run:implementation:pane-1",
    "close-pane:pane-1",
    "close-workspace:workspace-1",
  ]);
});

test("closes the pane when an agent run fails", async () => {
  const provider = new FakeWorkspaceProvider();
  const workspace = await provider.createWorkspace({
    taskId: "task-1",
    repositoryPath: "/repo",
    baseBranch: "main",
  });
  const runner: AgentRunner = {
    async run(_input): Promise<AgentRunResult> {
      throw new Error("runner failed");
    },
  };

  await expect(runAgentInPane(provider, runner, {
    taskId: "task-1",
    role: "verification",
    prompt: "Verify the change",
    workspace,
    signal: new AbortController().signal,
    onEvent: () => {},
  })).rejects.toThrow("runner failed");

  expect(provider.calls.at(-1)).toBe("close-pane:pane-1");
});

test("propagates cancellation to the runner and closes its pane", async () => {
  const provider = new FakeWorkspaceProvider();
  const workspace = await provider.createWorkspace({
    taskId: "task-1",
    repositoryPath: "/repo",
    baseBranch: "main",
  });
  const controller = new AbortController();
  let started!: () => void;
  const runnerStarted = new Promise<void>((resolve) => { started = resolve; });
  const runner: AgentRunner = {
    async run({ signal }) {
      return new Promise((_resolve, reject) => {
        const abort = () => reject(new DOMException("Cancelled", "AbortError"));
        signal.addEventListener("abort", abort, { once: true });
        if (signal.aborted) abort();
        started();
      });
    },
  };

  const run = runAgentInPane(provider, runner, {
    taskId: "task-1",
    role: "implementation",
    prompt: "Implement the task",
    workspace,
    signal: controller.signal,
    onEvent: () => {},
  });
  await runnerStarted;
  controller.abort();

  await expect(run).rejects.toHaveProperty("name", "AbortError");
  expect(provider.calls.at(-1)).toBe("close-pane:pane-1");
});
