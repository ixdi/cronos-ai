import type {
  AgentRole,
  AgentRunner,
  AgentRunEvent,
  AgentRunResult,
  TaskWorkspace,
  WorkspaceProvider,
} from "./contracts";

export type RunAgentInPaneInput = {
  taskId: string;
  role: AgentRole;
  prompt: string;
  workspace: TaskWorkspace;
  signal: AbortSignal;
  onEvent(event: AgentRunEvent): void;
};

export async function runAgentInPane(
  workspaceProvider: WorkspaceProvider,
  agentRunner: AgentRunner,
  input: RunAgentInPaneInput,
): Promise<AgentRunResult> {
  if (input.signal.aborted) {
    throw new DOMException("Agent run was cancelled", "AbortError");
  }

  const pane = await workspaceProvider.openPane(input.workspace, input.role);
  try {
    return await agentRunner.run({ ...input, pane });
  } finally {
    await workspaceProvider.closePane(pane);
  }
}
