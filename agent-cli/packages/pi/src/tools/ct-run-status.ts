import { type Static, Type } from "typebox";

import type { RunState } from "../client.ts";
import type { CloudThinkerRuntime } from "../runtime.ts";
import { askSummary, renderRun } from "./ct-ask.ts";
import { CT_ASK, CT_RUN_STATUS } from "./names.ts";
import { callComponent, callLine, legendLine, resultBody, summaryComponent } from "./render.ts";
import { text } from "./shared.ts";

const parameters = Type.Object({
	run_id: Type.String({ description: `The run_id ${CT_ASK} returned.` }),
});

const description = [
	`Read an Anna run started by ${CT_ASK}.`,
	`Use it after a run paused for approval, or after ${CT_ASK} returned before the run finished.`,
	"Returns the answer once the run has succeeded.",
].join("\n");

export function registerRunStatus(runtime: CloudThinkerRuntime): void {
	runtime.pi.registerTool<typeof parameters, RunState>({
		name: CT_RUN_STATUS,
		label: "Anna run status",
		description,
		promptSnippet: `Read an Anna run started by ${CT_ASK}`,
		parameters,
		execute: async (
			_toolCallId: string,
			params: Static<typeof parameters>,
			signal: AbortSignal | undefined,
		) => {
			runtime.requireSession();
			const state = await runtime.client.getRun(params.run_id, signal);
			if (runtime.approvalRunId === state.run_id) {
				if (state.status === "required_approval") {
					if (state.web_url) runtime.awaitApproval(state.run_id, state.web_url);
				} else {
					runtime.clearApproval();
				}
			}
			return text(renderRun(state), state);
		},
		renderCall: (params: Static<typeof parameters>, theme, context) =>
			callComponent(callLine(theme, CT_RUN_STATUS, params.run_id ?? ""), undefined, legendLine(theme, context.toolCallId, runtime.legend)),
		renderResult: (result, options, theme) =>
			summaryComponent(
				theme,
				result.details ? askSummary(result.details, theme) : "failed",
				resultBody(result),
				options.expanded,
			),
	});
}
