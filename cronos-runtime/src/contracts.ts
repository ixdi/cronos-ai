export type AgentRole =
  | "triage"
  | "implementation"
  | "code-review"
  | "verification"
  | "documentation";

export type TaskWorkspace = {
  id: string;
  taskId: string;
  rootPath: string;
};

export type AgentPane = {
  id: string;
  workspaceId: string;
};

export type AgentRunEvent = {
  type: "started" | "progress" | "completed" | "failed";
  message: string;
};

export type AgentRunResult = {
  summary: string;
  changedFiles: string[];
};

export type CreateWorkspaceInput = {
  taskId: string;
  repositoryPath: string;
  baseBranch: string;
};

export interface WorkspaceProvider {
  createWorkspace(input: CreateWorkspaceInput): Promise<TaskWorkspace>;
  restoreWorkspace(workspace: TaskWorkspace, repositoryPath: string): Promise<TaskWorkspace>;
  suspendWorkspace(workspace: TaskWorkspace): Promise<void>;
  openPane(workspace: TaskWorkspace, role: AgentRole): Promise<AgentPane>;
  closePane(pane: AgentPane): Promise<void>;
  closeWorkspace(workspace: TaskWorkspace): Promise<void>;
}

export type AgentRunInput = {
  taskId: string;
  role: AgentRole;
  prompt: string;
  workspace: TaskWorkspace;
  pane: AgentPane;
  signal: AbortSignal;
  onEvent(event: AgentRunEvent): void;
};

export interface AgentRunner {
  run(input: AgentRunInput): Promise<AgentRunResult>;
}
