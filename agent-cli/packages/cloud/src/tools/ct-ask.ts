import { type Static, Type } from "typebox";

import type { ExtensionContext, Theme } from "@earendil-works/pi-coding-agent";

import {
	CloudThinkerApiError,
	type RunApproval,
	type RunDecision,
	type RunPendingApproval,
	type RunState,
} from "../client.ts";
import { terminalNotify } from "../notify.ts";
import {
	ASK_THREAD_ENTRY_TYPE,
	type CloudThinkerRuntime,
	NOTIFY_HINT,
	normalizeAgentReference,
} from "../runtime.ts";
import { CT_ASK, CT_SANDBOX_READ, CT_SANDBOX_WRITE, CT_RUN_STATUS } from "./names.ts";
import {
	type Elapsed,
	callComponent,
	callLine,
	firstLine,
	link,
	resultBody,
	summaryComponent,
} from "./render.ts";
import { ApprovalPrompt } from "./approval-prompt.ts";
import {
	APPROVAL_NOTIFICATION,
	APPROVE_HERE,
	DECIDE_IN_BROWSER,
	DECISION_OPTIONS,
	approvalChoices,
} from "./ct-sandbox-write.ts";
import { pollUntil, text } from "./shared.ts";

export const POLL_INTERVAL_MS = 3_000;
export const MAX_WAIT_MS = 10 * 60 * 1000;
export const ASK_WORKING_MESSAGE = "Asking CloudThinker Agent…";
export const RUN_APPROVAL_TITLE = "CloudThinker Agent needs your approval";
export const RUN_NOT_AN_APPROVER_MESSAGE =
	"Only a workspace approver can approve this step. Ask one to open the link in the browser.";
export const RUN_NOT_DECIDABLE_MESSAGE =
	"That approval was not applied: the run moved on. Reading it again.";

export function askSummary(state: RunState, theme: Theme): string {
	if (state.status === "succeeded") return "CloudThinker Agent answered";
	if (state.status === "failed") return "failed";
	if (state.status === "required_approval") {
		const url = state.web_url;
		return `waiting for approval in browser${url ? ` → ${link(theme, url)}` : ""}`;
	}
	return `CloudThinker Agent is ${state.status}`;
}

const parameters = Type.Object({
	prompt: Type.String({
		description:
			"The question or instruction for CloudThinker Agent in the cloud. Include the " +
			"context, requested result, constraints, and evidence needed.",
	}),
});

const description = [
	"Delegate cloud work that needs judgment or several steps to CloudThinker Agent in the cloud.",
	"It runs on the workspace machine with workspace tools, memory, and incident history.",
	"",
	"Use it for:",
	"- open-ended investigations that are not one command: why a latency or cost jumped, what changed at a time, which service owns a symptom.",
	"- multi-step cloud changes that need judgment between steps. A human approves each write, in the terminal or the browser, before it runs.",
	"",
	`A single read-only lookup uses ${CT_SANDBOX_READ}; a single state-changing command uses ${CT_SANDBOX_WRITE}.`,
	`If approval is required, this returns a link; call ${CT_RUN_STATUS} later with the run_id to read the answer.`,
].join("\n");

export function renderRun(state: RunState): string {
	const lines = [`run_id: ${state.run_id}`, `status: ${state.status}`];
	if (state.web_url) lines.push(`web_url: ${state.web_url}`);
	if (state.status === "succeeded") {
		lines.push("", state.answer ?? "(No answer text returned.)");
		return lines.join("\n");
	}
	if (state.status === "required_approval") {
		lines.push(
			"",
			`A human must approve this in the browser at ${state.web_url ?? "the link above"}. Nobody has been notified yet; ${NOTIFY_HINT}.`,
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

export interface RunApprover extends RunReader {
	getRunApproval(runId: string, signal?: AbortSignal): Promise<RunApproval>;
	decideRun(
		runId: string,
		body: { tool_call_id: string; decision: "approve" | "decline"; reason?: string },
		signal?: AbortSignal,
	): Promise<RunDecision>;
}

type RunAnswer =
	| { decision: "approve" }
	| { decision: "decline"; reason?: string }
	| { decision: "browser" };

async function askRunInTerminal(
	ctx: Pick<ExtensionContext, "hasUI" | "ui"> & Partial<Pick<ExtensionContext, "mode">>,
	pending: RunPendingApproval,
	webUrl: string | null,
): Promise<RunAnswer> {
	const reasoning = pending.reason || `CloudThinker Agent wants to run ${pending.tool_name}.`;
	let choice: string | undefined;
	if (ctx.mode === "tui") {
		terminalNotify(APPROVAL_NOTIFICATION);
		choice = await ctx.ui.custom<string | undefined>((tui, theme, _keybindings, done) => new ApprovalPrompt({
			title: RUN_APPROVAL_TITLE,
			reasoning,
			connections: pending.tool_name,
			script: pending.arguments,
			webUrl: webUrl ?? "",
			choices: approvalChoices(DECISION_OPTIONS),
		}, theme, done, () => tui.requestRender(), () => tui.terminal.rows));
	} else {
		choice = await ctx.ui.select(`CloudThinker Agent: ${firstLine(reasoning)}`, [...DECISION_OPTIONS]);
	}
	if (choice === APPROVE_HERE) return { decision: "approve" };
	if (choice === DECIDE_IN_BROWSER) return { decision: "browser" };
	if (choice === undefined) return { decision: "decline" };
	const reason = await ctx.ui.input("Why decline? (optional)", "reason");
	return { decision: "decline", reason: reason?.trim() || undefined };
}

export interface RunApprovalOptions {
	client: RunApprover;
	ctx: Pick<ExtensionContext, "hasUI" | "ui"> & Partial<Pick<ExtensionContext, "mode">>;
	state: RunState;
	warn: (message: string) => void;
	signal?: AbortSignal;
	onTick?: (state: RunState, elapsedMs: number) => void;
	intervalMs?: number;
	maxWaitMs?: number;
	now?: () => number;
}

export async function approveRunInTerminal(options: RunApprovalOptions): Promise<RunState> {
	const { client, ctx, signal } = options;
	const intervalMs = options.intervalMs ?? POLL_INTERVAL_MS;
	const maxWaitMs = options.maxWaitMs ?? MAX_WAIT_MS;
	let state = options.state;
	while (ctx.hasUI && state.status === "required_approval") {
		const approval = await client.getRunApproval(state.run_id, signal);
		const pending = approval.pending;
		if (approval.status !== "required_approval") {
			state = await pollRun({ ...options, runId: state.run_id });
			continue;
		}
		if (!pending?.decidable) return state;
		const answer = await askRunInTerminal(ctx, pending, approval.web_url ?? state.web_url);
		if (answer.decision === "browser") return state;
		try {
			await client.decideRun(
				state.run_id,
				{
					tool_call_id: pending.tool_call_id,
					decision: answer.decision,
					...(answer.decision === "decline" && answer.reason ? { reason: answer.reason } : {}),
				},
				signal,
			);
		} catch (error) {
			if (!(error instanceof CloudThinkerApiError)) throw error;
			if (error.status === 403) {
				options.warn(RUN_NOT_AN_APPROVER_MESSAGE);
				return state;
			}
			if (error.status !== 409) throw error;
			options.warn(RUN_NOT_DECIDABLE_MESSAGE);
			continue;
		}
		const moved = await pollUntil({
			read: (readSignal) => client.getRunApproval(state.run_id, readSignal),
			settled: (current) =>
				current.status !== "required_approval" ||
				(current.pending !== null && current.pending.tool_call_id !== pending.tool_call_id),
			intervalMs,
			maxWaitMs,
			signal,
			now: options.now,
		});
		if (moved.status === "required_approval" && moved.pending?.tool_call_id === pending.tool_call_id) {
			return client.getRun(state.run_id, signal);
		}
		state = await pollRun({ ...options, runId: state.run_id });
	}
	return state;
}

export function submitBody(
	prompt: string,
	sessionConversationId: string,
	thread: { conversation_id: string } | undefined,
	selectedAgentReference?: string,
): {
	prompt: string;
	selection: { option_id: string };
	conversation_id?: string;
	source_conversation_id?: string;
	selected_agent_reference?: string;
} {
	const selection = { option_id: "mode:pro" };
	const selected = normalizeAgentReference(selectedAgentReference);
	const identity = selected ? { selected_agent_reference: selected } : {};
	return thread
		? { prompt, selection, ...identity, conversation_id: thread.conversation_id }
		: { prompt, selection, ...identity, source_conversation_id: sessionConversationId };
}

export function registerAsk(runtime: CloudThinkerRuntime): void {
	runtime.pi.registerTool<typeof parameters, RunState & Elapsed>({
		name: CT_ASK,
		label: "Ask CloudThinker Agent",
		description,
		promptSnippet: "Delegate cloud work that needs judgment or several steps",
		promptGuidelines: [
			`Never run a state-changing cloud operation through ${CT_SANDBOX_READ} and never ask the user to run it themselves; one command goes to ${CT_SANDBOX_WRITE}, multi-step work to ${CT_ASK}.`,
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
				const selectedAgentReference = normalizeAgentReference(
					runtime.selectedAgentReference ?? runtime.askThread?.selected_agent_reference,
				);
				const submitted = await runtime.client.submitRun(
					submitBody(
						params.prompt,
						session.conversation_id,
						runtime.askThread,
						selectedAgentReference,
					),
					signal,
				);
				if (!runtime.askThread) {
					runtime.askThread = {
						conversation_id: submitted.conversation_id,
						web_url: submitted.web_url,
						...(selectedAgentReference
							? { selected_agent_reference: selectedAgentReference }
							: {}),
					};
					runtime.pi.appendEntry(ASK_THREAD_ENTRY_TYPE, runtime.askThread);
				}
				const onTick = (current: RunState, elapsedMs: number): void => {
					onUpdate?.(
						text(
							`CloudThinker Agent is ${current.status} (${Math.round(elapsedMs / 1000)}s).`,
							{ ...current, elapsed_ms: elapsedMs },
						),
					);
				};
				const polled = await pollRun({
					client: runtime.client,
					runId: submitted.run_id,
					signal,
					onTick,
				});
				const state = await approveRunInTerminal({
					client: runtime.client,
					ctx,
					state: polled,
					warn: (message) => runtime.notify(message, "warning"),
					signal,
					onTick,
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
		renderCall: (params: Static<typeof parameters>, theme, context) =>
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
