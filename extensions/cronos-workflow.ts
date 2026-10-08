import { Type } from "@earendil-works/pi-ai";
import { defineTool, type ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { execFile } from "node:child_process";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);

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
			const value = JSON.parse(stdout);
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

export default function (pi: ExtensionAPI) {
	pi.registerTool(taskStateTool);
}
