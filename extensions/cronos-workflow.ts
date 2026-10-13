import { StringEnum, Type } from "@earendil-works/pi-ai";
import { defineTool, type ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { execFile } from "node:child_process";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);
// Task records may be up to 1 MiB on disk (storage MAX_JSON_BYTES); leave room for pretty-printing.
const TASK_JSON_BUFFER = 2 * 1024 * 1024;

const agentSteps = [
	"specification-generation",
	"code-generation",
	"code-review",
	"verification",
	"documentation",
] as const;

// Findings for the next revision must survive several reviewers plus the check command, so keep
// the current attempt's recent events rather than a fixed handful.
function currentAttemptActivity(value: any) {
	return (value.agent_activity ?? [])
		.filter((event: any) => event.attempt === value.attempt)
		.slice(-24);
}

function taskSnapshot(value: any) {
	return {
		id: value.id,
		title: value.title,
		description: value.description,
		priority: value.priority,
		dependencies: value.dependencies,
		status: value.status,
		attempt: value.attempt,
		automatic_revisions: value.automatic_revisions ?? 0,
		openspec_change: `cronos-${String(value.id).replaceAll("-", "").slice(0, 8)}`,
		current_step: value.current_step,
		step_started_at_unix_secs: value.step_started_at_unix_secs,
		agent_activity: currentAttemptActivity(value),
		approval_history: value.approval_history?.slice(-6) ?? [],
		recovery_history: value.recovery_history?.slice(-5).map((record: any) => ({
			attempt: record.attempt,
			action: record.action,
			note: record.note,
		})) ?? [],
		abandonment_note: value.abandonment_note,
	};
}

const agentProfilesTool = defineTool({
	name: "cronos_agent_profiles",
	label: "Cronos agent profiles",
	description: "List named Pi resource profiles permitted for a Cronos workflow step. Call before assigning profiles to step agents.",
	parameters: Type.Object({ step: StringEnum(agentSteps) }),
	async execute(_toolCallId, params, signal) {
		const binary = process.env.CRONOS_AI_BIN || "cronos-ai";
		const args = [
			...(process.env.CRONOS_AI_DATA_DIR
				? ["--data-dir", process.env.CRONOS_AI_DATA_DIR]
				: []),
			"internal",
			"agent-profiles",
			params.step,
		];
		try {
			const { stdout } = await execFileAsync(binary, args, {
				signal,
				timeout: 10_000,
				maxBuffer: 128 * 1024,
			});
			const profiles = JSON.parse(stdout);
			return {
				content: [{ type: "text", text: JSON.stringify(profiles) }],
				details: profiles,
			};
		} catch (error) {
			const message = error instanceof Error ? error.message : String(error);
			return {
				content: [{ type: "text", text: `Cronos agent profile lookup failed: ${message}` }],
				details: { error: message },
				isError: true,
			};
		}
	},
});

const taskStateParameters = Type.Object({
	project_id: Type.String({ minLength: 36, maxLength: 36 }),
	task_id: Type.String({ minLength: 36, maxLength: 36 }),
});

const taskStateTool = defineTool<
	typeof taskStateParameters,
	ReturnType<typeof taskSnapshot> | { error: string }
>({
	name: "cronos_task_state",
	label: "Cronos task state",
	description: "Read persisted state for the active Cronos-AI task.",
	parameters: taskStateParameters,
	async execute(_toolCallId, params, signal) {
		const binary = process.env.CRONOS_AI_BIN || "cronos-ai";
		const args = [
			...(process.env.CRONOS_AI_DATA_DIR
				? ["--data-dir", process.env.CRONOS_AI_DATA_DIR]
				: []),
			"internal",
			"task",
			"get",
			params.project_id,
			params.task_id,
		];

		try {
			const { stdout } = await execFileAsync(binary, args, {
				signal,
				timeout: 10_000,
				maxBuffer: TASK_JSON_BUFFER,
			});
			const value = taskSnapshot(JSON.parse(stdout));
			return {
				content: [{ type: "text", text: JSON.stringify(value) }],
				details: value,
			};
		} catch (error) {
			const message = error instanceof Error ? error.message : String(error);
			return {
				content: [{ type: "text", text: `Cronos task-state request failed: ${message}` }],
				details: { error: message },
				isError: true,
			};
		}
	},
});

const runStepParameters = Type.Object({
	project_id: Type.String({ minLength: 36, maxLength: 36 }),
	task_id: Type.String({ minLength: 36, maxLength: 36 }),
	step: StringEnum(agentSteps),
	agents: Type.Array(
		Type.Object({
			name: Type.String({ minLength: 1, maxLength: 32 }),
			prompt: Type.String({ minLength: 1, maxLength: 20_000 }),
			profile_id: Type.Optional(Type.String({ minLength: 1, maxLength: 64 })),
		}),
		{ minItems: 1, maxItems: 4 },
	),
});

const runStepTool = defineTool<
	typeof runStepParameters,
	| { status: string; attempt: number; current_step: string | null; agent_activity: unknown[] }
	| { error: string }
>({
	name: "cronos_run_step",
	label: "Run Cronos workflow step",
	description:
		"Run the current agent step with Pi sub-agents. Specification and code generation take exactly one agent; code review, verification, and documentation take one to four independent agents. Call cronos_agent_profiles first and assign a returned profile_id only when its resources fit that agent. Cronos appends its own contract (result file, OpenSpec change, write rules) to each prompt, validates ordering, profiles, approvals, and the worktree, waits for every agent, and decides the next step from their structured results.",
	parameters: runStepParameters,
	async execute(_toolCallId, params, signal) {
		const binary = process.env.CRONOS_AI_BIN || "cronos-ai";
		const args = [
			...(process.env.CRONOS_AI_DATA_DIR
				? ["--data-dir", process.env.CRONOS_AI_DATA_DIR]
				: []),
			"internal",
			"workflow",
			"run-step",
			params.project_id,
			params.task_id,
			params.step,
			JSON.stringify(params.agents),
		];
		try {
			// No client timeout: Rust bounds every agent with its step or profile time limit.
			const { stdout } = await execFileAsync(binary, args, {
				signal,
				maxBuffer: TASK_JSON_BUFFER,
			});
			const value = JSON.parse(stdout);
			const snapshot = {
				status: value.status,
				attempt: value.attempt,
				current_step: value.current_step,
				agent_activity: currentAttemptActivity(value),
			};
			return {
				content: [{ type: "text", text: JSON.stringify(snapshot) }],
				details: snapshot,
			};
		} catch (error) {
			const message = error instanceof Error ? error.message : String(error);
			return {
				content: [{ type: "text", text: `Cronos workflow step failed: ${message}` }],
				details: { error: message },
				isError: true,
			};
		}
	},
});

export default function (pi: ExtensionAPI) {
	pi.registerTool(agentProfilesTool);
	pi.registerTool(taskStateTool);
	pi.registerTool(runStepTool);

	pi.on("before_agent_start", (event) => ({
		systemPrompt: `${event.systemPrompt}\n\nCronos-AI workflow instructions:\n- Read the assigned task using cronos_task_state before acting. On a clarified attempt, apply only the clarification note recorded for the current attempt; older attempts are history, not active instructions.\n- You may use the read tool to inspect the task worktree (for example the task's OpenSpec change under openspec/changes/ and the code) to write specific prompts. You cannot modify files.\n- Run the task's current agent step using cronos_run_step, then read the returned state and keep running steps while the current step is an agent step. Stop when the task reaches an approval gate, local-merge, Error, or Completed.\n- When the task returns to code-generation after review, verification, or a merge conflict, pass the recorded findings (agent_activity with status changes_requested) to the code-generation agent.\n- Do not bypass the specification or final-code approval gates; the human handles those in the Cronos TUI. Rust archives the OpenSpec change, makes the documentation commit, and performs the local merge; nobody pushes.`,
	}));

	pi.on("session_start", async () => {
		const projectId = process.env.CRONOS_PROJECT_ID;
		const taskId = process.env.CRONOS_TASK_ID;
		if (projectId && taskId) {
			await pi.sendUserMessage(
				`Orchestrate Cronos-AI task ${taskId} in project ${projectId}. Read its state and continue strictly from the current workflow step.`,
			);
		}
	});
}
