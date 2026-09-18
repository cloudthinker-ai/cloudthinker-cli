import { type Static, Type } from "typebox";

import type { ExecutionOutput } from "../client.ts";
import type { CloudThinkerRuntime } from "../runtime.ts";
import { CT_SANDBOX_READ, READ_TASK_OUTPUT } from "./names.ts";
import { callComponent, callLine, legendLine, resultBody, summaryComponent } from "./render.ts";
import { section, text } from "./shared.ts";

const parameters = Type.Object({
	task_id: Type.String({
		description: `The task_id ${CT_SANDBOX_READ} returned for a background run.`,
	}),
	since: Type.Optional(
		Type.Integer({
			minimum: 0,
			description:
				"Output cursor. Send 0 on the first poll, then the next_cursor of " +
				"the previous poll to read only what is new.",
		}),
	),
});

const description = [
	`Read the output of a background ${CT_SANDBOX_READ} run.`,
	"Poll until status is done, error, or cancelled; while it is running the output is whatever has been written so far.",
	"Pass the previous next_cursor as since so each poll returns only new output.",
].join("\n");

export function renderOutput(result: ExecutionOutput): string {
	const header = [`status: ${result.status}`, `next_cursor: ${result.next_cursor}`];
	if (result.truncated) header.push("truncated: true");
	if (result.exit_code !== undefined && result.exit_code !== null) {
		header.push(`exit_code: ${result.exit_code}`);
	}
	if (result.termination_reason) {
		header.push(`termination_reason: ${result.termination_reason}`);
	}
	return [header.join("\n"), "", section("output", result.output)].join("\n");
}

export function registerReadTaskOutput(runtime: CloudThinkerRuntime): void {
	runtime.pi.registerTool<typeof parameters, ExecutionOutput>({
		name: READ_TASK_OUTPUT,
		label: "Cloud task output",
		description,
		promptSnippet: "Read the output of a background cloud task",
		parameters,
		execute: async (
			_toolCallId: string,
			params: Static<typeof parameters>,
			signal: AbortSignal | undefined,
		) => {
			const session = runtime.requireSession();
			const result = await runtime.client.readExecution(
				params.task_id,
				{ conversation_id: session.conversation_id, since: params.since ?? 0 },
				signal,
			);
			return text(renderOutput(result), result);
		},
		renderCall: (params: Static<typeof parameters>, theme, context) =>
			callComponent(callLine(theme, READ_TASK_OUTPUT, params.task_id ?? ""), undefined, legendLine(theme, context.toolCallId, runtime.legend)),
		renderResult: (result, options, theme) =>
			summaryComponent(
				theme,
				`cloud task ${result.details?.status ?? "unknown"}`,
				resultBody(result),
				options.expanded,
			),
	});
}
