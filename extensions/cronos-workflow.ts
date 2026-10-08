import { StringEnum, Type } from "@earendil-works/pi-ai";
import { defineTool, type ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { execFile } from "node:child_process";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);

const agentSteps = [
	"specification-generation",
	"code-generation",
	"code-review",
	"verification",
	"documentation",
] as const;

function taskSnapshot(value: any) {
	return {
		id: value.id,
		title: value.title,
		description: value.description,
		priority: value.priority,
		dependencies: value.dependencies,
		status: value.status,
		current_step: value.current_step,
		step_started_at_unix_secs: value.step_started_at_unix_secs,
		agent_activity: value.agent_activity?.slice(-8) ?? [],
		approval_history: value.approval_history?.slice(-6) ?? [],
		abandonment_note: value.abandonment_note,
	};
}

const taskStateTool = defineTool({
	name: "cronos_task_state",
	label: "Cronos task state",
	description: "Read persisted state for the active Cronos-AI task.",
	parameters: Type.Object({
		project_id: Type.String({ minLength: 36, maxLength: 36 }),
		task_id: Type.String({ minLength: 36, maxLength: 36 }),
	}),
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
				maxBuffer: 128 * 1024,
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

const runStepTool = defineTool({
	name: "cronos_run_step",
	label: "Run Cronos workflow step",
	description:
		"Run the current agent step with independent Pi sub-agents. Cronos validates ordering, approval gates, task status, and the task worktree, then waits for every result before advancing.",
	parameters: Type.Object({
		project_id: Type.String({ minLength: 36, maxLength: 36 }),
		task_id: Type.String({ minLength: 36, maxLength: 36 }),
		step: StringEnum(agentSteps),
		agents: Type.Array(
			Type.Object({
				name: Type.String({ minLength: 1, maxLength: 32 }),
				prompt: Type.String({ minLength: 1, maxLength: 20_000 }),
			}),
			{ minItems: 1, maxItems: 4 },
		),
	}),
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
			const { stdout } = await execFileAsync(binary, args, {
				signal,
				timeout: 180_000,
				maxBuffer: 512 * 1024,
			});
			const value = JSON.parse(stdout);
			const snapshot = {
				status: value.status,
				current_step: value.current_step,
				agent_activity: value.agent_activity?.slice(-8) ?? [],
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
	pi.registerTool(taskStateTool);
	pi.registerTool(runStepTool);

	pi.on("before_agent_start", (event) => ({
		systemPrompt: `${event.systemPrompt}\n\nCronos-AI workflow instructions:\n- Read the assigned task using cronos_task_state before acting.\n- Run only the task's current agent step using cronos_run_step; Rust validates status, step order, approvals, and the task worktree.\n- Wait for each step tool call to finish before continuing. Do not bypass the specification or final-code approval gates; the human handles those in the Cronos TUI.\n- Do not modify task status, approval records, or worktree paths directly.`,
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
