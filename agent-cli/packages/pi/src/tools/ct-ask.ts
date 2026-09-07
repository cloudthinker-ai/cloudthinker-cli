import { type Static, Type } from "typebox";

import type { Theme } from "@earendil-works/pi-coding-agent";

import type { RunState } from "../client.ts";
import {
	ASK_THREAD_ENTRY_TYPE,
	type CloudThinkerRuntime,
	NOTIFY_HINT,
} from "../runtime.ts";
import { CT_ASK, CT_CLOUD_READ, CT_CLOUD_WRITE, CT_RUN_STATUS } from "./names.ts";
import {
	type Elapsed,
	callComponent,
	callLine,
	firstLine,
	link,
	resultBody,
	summaryComponent,
} from "./render.ts";
import { pollUntil, text } from "./shared.ts";

export const POLL_INTERVAL_MS = 3_000;
export const MAX_WAIT_MS = 10 * 60 * 1000;
export const ASK_WORKING_MESSAGE = "Asking Anna…";

export function askSummary(state: RunState, theme: Theme): string {
	if (state.status === "succeeded") return "Anna answered";
	if (state.status === "failed") return "failed";
	if (state.status === "required_approval") {
		const url = state.web_url;
		return `waiting for approval in browser${url ? ` → ${link(theme, url)}` : ""}`;
	}
	return `Anna is ${state.status}`;
}

const parameters = Type.Object({
	prompt: Type.String({
		description:
			"The question or instruction for Anna. Say what you already know and " +
			"what you need back, the way you would brief a teammate.",
	}),
});

const description = [
	"Ask Anna, CloudThinker's SuperAgent, to do cloud work this session cannot do itself.",
	"She runs in the workspace with every Core tool, the workspace memory, and the incident history.",
	"",
	"Send her:",
	"- open-ended investigations that are not one command: why a latency or cost jumped, what changed at a time, which service owns a symptom.",
	"- multi-step cloud changes that need her judgment between steps. A human approves each write in the browser before it runs.",
	"",
	`A single read-only lookup is faster and cheaper through ${CT_CLOUD_READ}, and a single state-changing command through ${CT_CLOUD_WRITE}; use ${CT_ASK} when a command is not the shape of the answer.`,
	`If she pauses for approval this returns immediately with a link for the user; call ${CT_RUN_STATUS} later with the run_id to pick the answer up.`,
].join("\n");

export function renderRun(state: RunState): string {
	const lines = [`run_id: ${state.run_id}`, `status: ${state.status}`];
	if (state.web_url) lines.push(`web_url: ${state.web_url}`);
	if (state.status === "succeeded") {
		lines.push("", state.answer ?? "(Anna returned no answer text.)");
		return lines.join("\n");
	}
	if (state.status === "required_approval") {
		lines.push(
			"",
			`Anna is waiting for a human to approve this in the browser at ${state.web_url ?? "the link above"}. Nobody has been told yet; ${NOTIFY_HINT}.`,
			`Tell the user, then call ${CT_RUN_STATUS} with this run_id to read the answer.`,
		);
		return lines.join("\n");
	}
	if (state.status === "failed") {
		if (state.failure_kind) lines.push(`failure_kind: ${state.failure_kind}`);
		if (state.message) lines.push("", state.message);
		return lines.join("\n");
	}
	lines.push(
		"",
		`Still running. Call ${CT_RUN_STATUS} with this run_id to check again.`,
	);
	return lines.join("\n");
}

export interface RunReader {
	getRun(runId: string, signal?: AbortSignal): Promise<RunState>;
}

export interface PollOptions {
	client: RunReader;
	runId: string;
	signal?: AbortSignal;
	onTick?: (state: RunState, elapsedMs: number) => void;
	intervalMs?: number;
	maxWaitMs?: number;
	now?: () => number;
}

export function runSettled(state: RunState): boolean {
	return (
		state.status === "succeeded" ||
		state.status === "failed" ||
		state.status === "required_approval"
	);
}

export function pollRun(options: PollOptions): Promise<RunState> {
	return pollUntil({
		read: (signal) => options.client.getRun(options.runId, signal),
		settled: runSettled,
		intervalMs: options.intervalMs ?? POLL_INTERVAL_MS,
		maxWaitMs: options.maxWaitMs ?? MAX_WAIT_MS,
		onTick: options.onTick,
		signal: options.signal,
		now: options.now,
	});
}

export function submitBody(
	prompt: string,
	sessionConversationId: string,
	thread: { conversation_id: string } | undefined,
): { prompt: string; conversation_id?: string; source_conversation_id?: string } {
	return thread
		? { prompt, conversation_id: thread.conversation_id }
		: { prompt, source_conversation_id: sessionConversationId };
}

export function registerAsk(runtime: CloudThinkerRuntime): void {
	runtime.pi.registerTool<typeof parameters, RunState & Elapsed>({
		name: CT_ASK,
		label: "Ask Anna",
		description,
		promptSnippet:
			"Ask CloudThinker's SuperAgent Anna for a cloud write or an open-ended investigation",
		promptGuidelines: [
			`Never run a state-changing cloud operation through ${CT_CLOUD_READ} and never ask the user to run it themselves; one command goes to ${CT_CLOUD_WRITE}, multi-step work to ${CT_ASK}.`,
		],
		parameters,
		execute: async (
			_toolCallId: string,
			params: Static<typeof parameters>,
			signal: AbortSignal | undefined,
			onUpdate,
			ctx,
		) => {
			const session = runtime.requireSession();
			runtime.clearApproval();
			const startedAt = Date.now();
			if (ctx.hasUI) ctx.ui.setWorkingMessage(ASK_WORKING_MESSAGE);
			try {
				const submitted = await runtime.client.submitRun(
					submitBody(params.prompt, session.conversation_id, runtime.askThread),
					signal,
				);
				if (!runtime.askThread) {
					runtime.askThread = {
						conversation_id: submitted.conversation_id,
						web_url: submitted.web_url,
					};
					runtime.pi.appendEntry(ASK_THREAD_ENTRY_TYPE, runtime.askThread);
				}
				const state = await pollRun({
					client: runtime.client,
					runId: submitted.run_id,
					signal,
					onTick: (current, elapsedMs) => {
						onUpdate?.(
							text(
								`Anna is ${current.status} (${Math.round(elapsedMs / 1000)}s).`,
								{ ...current, elapsed_ms: elapsedMs },
							),
						);
					},
				});
				if (state.status === "required_approval" && state.web_url) {
					runtime.awaitApproval(state.run_id, state.web_url);
				}
				return text(renderRun(state), {
					...state,
					elapsed_ms: Date.now() - startedAt,
				});
			} finally {
				if (ctx.hasUI) ctx.ui.setWorkingMessage();
			}
		},
		renderCall: (params: Static<typeof parameters>, theme) =>
			callComponent(callLine(theme, CT_ASK, firstLine(params.prompt ?? ""))),
		renderResult: (result, options, theme) =>
			summaryComponent(
				theme,
				result.details ? askSummary(result.details, theme) : "failed",
				resultBody(result),
				options.expanded,
			),
	});
}
