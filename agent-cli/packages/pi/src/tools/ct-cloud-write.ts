import { type Static, Type } from "typebox";

import type {
	AgentToolResult,
	ExtensionContext,
	SessionManager,
	Theme,
} from "@earendil-works/pi-coding-agent";

import { type CloudWrite, CloudThinkerApiError, type WriteOutcome } from "../client.ts";
import { APPROVAL_KEY, AUTO_MODE_OFF_REASON, type CloudThinkerRuntime } from "../runtime.ts";
import { DEFAULT_TIMEOUT_SECONDS, MAX_TIMEOUT_SECONDS, renderExecution } from "./ct-cloud-read.ts";
import { CT_ASK, CT_CLOUD_READ, CT_CLOUD_WRITE, READ_TASK_OUTPUT } from "./names.ts";
import {
	type Elapsed,
	callComponent,
	callLine,
	firstLine,
	formatElapsed,
	link,
	resultBody,
	summaryComponent,
} from "./render.ts";
import { explain, pollUntil, text } from "./shared.ts";

export const WRITE_POLL_INTERVAL_MS = 3_000;
export const WRITE_MAX_WAIT_MS = 10 * 60 * 1000;
export const WRITE_WORKING_MESSAGE = "Waiting for approval in the browser…";
export const RECENT_USER_MESSAGES = 3;
export const RECENT_USER_MESSAGE_CHARS = 2_000;
export const WRITE_SUBJECT = "A cloud write";
export const APPROVE_HERE = "Approve and run";
export const APPROVE_AND_TRUST = "Approve and trust this command";
export const DECLINE_HERE = "Decline";
export const DECIDE_IN_BROWSER = "Decide in the browser";
export const DECISION_OPTIONS = [APPROVE_HERE, DECLINE_HERE, DECIDE_IN_BROWSER];
export const TRUST_DECISION_OPTIONS = [
	APPROVE_HERE,
	APPROVE_AND_TRUST,
	DECLINE_HERE,
	DECIDE_IN_BROWSER,
];
export const NOT_AN_APPROVER_MESSAGE =
	"Only a workspace approver can approve a cloud write. Ask one to open the link in the browser.";
export const TRUST_REFUSED_MESSAGE =
	"The approval was not applied: only a workspace settings editor can trust a command. Decide again without trusting it.";

const parameters = Type.Object({
	connection_list: Type.Optional(
		Type.Array(Type.String(), {
			description:
				"Connection prefixes whose credentials the command needs, drawn ONLY " +
				"from the connected prefixes listed in the system prompt.",
		}),
	),
	script: Type.Optional(
		Type.String({
			description:
				"ONE state-changing shell command to run in the Sandbox: create, " +
				"delete, apply, scale, restart, rotate, put, tag. The Connection's CLI " +
				"is already installed and authenticated.",
		}),
	),
	reasoning: Type.Optional(
		Type.String({
			description:
				"One plain-language sentence for the human approver: what this " +
				"changes and why it is needed right now. No tool names, no jargon.",
		}),
	),
	timeout: Type.Optional(
		Type.Integer({
			minimum: 1,
			maximum: MAX_TIMEOUT_SECONDS,
			description: `Seconds to wait for the command, at most ${MAX_TIMEOUT_SECONDS}. Defaults to ${DEFAULT_TIMEOUT_SECONDS}.`,
		}),
	),
	run_in_background: Type.Optional(
		Type.Boolean({
			description: `Detach the command once it may run and poll ${READ_TASK_OUTPUT} for its output.`,
		}),
	),
	write_id: Type.Optional(
		Type.String({
			description:
				"Resume a write that was still waiting for approval: pass ONLY this " +
				"id, exactly as the earlier call returned it.",
		}),
	),
});

type Params = Static<typeof parameters>;

const description = [
	"Run ONE state-changing command in CloudThinker's cloud Sandbox with a workspace Connection's credential injected.",
	"The workspace decides whether it runs: a command the workspace already trusts runs at once; anything else pauses until a human approves it, in this terminal or in the browser, then runs here.",
	"",
	"The credential stays in the cloud. You never see it, you only get stdout back.",
	`Read-only lookups (describe, list, get, logs) belong in ${CT_CLOUD_READ}; open-ended, multi-step cloud work belongs in ${CT_ASK}.`,
	"",
	"Name the ONE command that makes the change; the approval card is where the human confirms it. Never batch several changes into one script.",
	"If the result says the write is still waiting, tell the user and stop; call this tool again later with only write_id to pick it up. Never re-send the same script while a write is waiting.",
	"If the result says declined or denied, do not retry it and do not work around it. Tell the user the reason.",
].join("\n");

export interface WriteReader {
	getWrite(writeId: string, signal?: AbortSignal): Promise<CloudWrite>;
}

export interface WritePollOptions {
	client: WriteReader;
	writeId: string;
	signal?: AbortSignal;
	onTick?: (write: CloudWrite, elapsedMs: number) => void;
	intervalMs?: number;
	maxWaitMs?: number;
	now?: () => number;
}

export function pollWrite(options: WritePollOptions): Promise<CloudWrite> {
	return pollUntil({
		read: (signal) => options.client.getWrite(options.writeId, signal),
		settled: (write) => write.status !== "required_approval",
		intervalMs: options.intervalMs ?? WRITE_POLL_INTERVAL_MS,
		maxWaitMs: options.maxWaitMs ?? WRITE_MAX_WAIT_MS,
		onTick: options.onTick,
		signal: options.signal,
		now: options.now,
	});
}

export function recentUserMessages(
	sessions: Pick<SessionManager, "getBranch">,
): string[] {
	const texts: string[] = [];
	for (const entry of sessions.getBranch()) {
		if (entry.type !== "message" || entry.message.role !== "user") continue;
		const content: unknown = entry.message.content;
		const body =
			typeof content === "string"
				? content
				: Array.isArray(content)
					? content
							.map((block: { type?: string; text?: string }) =>
								block.type === "text" ? (block.text ?? "") : "",
							)
							.join("")
							.trim()
					: "";
		if (body) texts.push(body.slice(0, RECENT_USER_MESSAGE_CHARS));
	}
	return texts.slice(-RECENT_USER_MESSAGES);
}

export function approvalCard(write: CloudWrite): string[] {
	const connections = write.connection_list.join(", ") || "no Connection";
	return [
		`⏸ ${WRITE_SUBJECT} is waiting for your approval`,
		`  ${write.reasoning}`,
		`  ☁ ${connections}: ${firstLine(write.script)}`,
		`  browser → ${write.web_url}`,
	];
}

export type TerminalDecision = "approve" | "decline" | "browser";

export interface TerminalAnswer {
	decision: TerminalDecision;
	reason?: string;
	trust?: boolean;
}

export async function askInTerminal(
	ctx: Pick<ExtensionContext, "hasUI" | "ui">,
	write: CloudWrite,
	offerTrust = true,
): Promise<TerminalAnswer> {
	if (!ctx.hasUI) return { decision: "browser" };
	ctx.ui.setWidget(APPROVAL_KEY, approvalCard(write), { placement: "aboveEditor" });
	const choice = await ctx.ui.select(
		`Cloud write: ${firstLine(write.reasoning)}`,
		offerTrust ? TRUST_DECISION_OPTIONS : DECISION_OPTIONS,
	);
	if (choice === APPROVE_HERE) return { decision: "approve" };
	if (choice === APPROVE_AND_TRUST) return { decision: "approve", trust: true };
	if (choice === DECIDE_IN_BROWSER) return { decision: "browser" };
	if (choice === undefined) return { decision: "decline" };
	const reason = await ctx.ui.input("Why decline? (optional)", "reason");
	return { decision: "decline", reason: reason?.trim() || undefined };
}

export interface WriteDecider {
	decideWrite(
		writeId: string,
		body: { decision: "approve" | "decline"; reason?: string; trust?: boolean },
		signal?: AbortSignal,
	): Promise<CloudWrite>;
}

export async function decideInTerminal(
	client: WriteDecider,
	ctx: Pick<ExtensionContext, "hasUI" | "ui">,
	write: CloudWrite,
	warn: (message: string) => void,
	signal?: AbortSignal,
): Promise<CloudWrite | undefined> {
	let offerTrust = true;
	for (;;) {
		const answer = await askInTerminal(ctx, write, offerTrust);
		if (answer.decision === "browser") return undefined;
		try {
			return await client.decideWrite(
				write.id,
				{ decision: answer.decision, reason: answer.reason, trust: answer.trust },
				signal,
			);
		} catch (error) {
			if (!(error instanceof CloudThinkerApiError) || error.status !== 403) throw error;
			if (!answer.trust) {
				warn(NOT_AN_APPROVER_MESSAGE);
				return undefined;
			}
			warn(TRUST_REFUSED_MESSAGE);
			offerTrust = false;
		}
	}
}

export function humaniseReason(reason: string): string {
	return reason === "workspace_trusted_command" ? "trusted command" : reason;
}

export function verdictLabel(write: Pick<CloudWrite, "verdict" | "verdict_reason">): string {
	if (write.verdict_reason === AUTO_MODE_OFF_REASON) return "manual: needs approval";
	if (write.verdict === "allow") return `auto: allowed (${humaniseReason(write.verdict_reason)})`;
	if (write.verdict === "require_approval") return "auto: needs approval";
	if (write.verdict === "escalate") return "auto: escalated";
	return "auto: denied";
}

function decidedBy(write: CloudWrite): string {
	return `${write.trusted ? "approved and trusted" : "approved"} by ${write.decided_by_name}`;
}

export function renderWrite(outcome: WriteOutcome): string {
	const { write, execution } = outcome;
	const lines = [
		`write_id: ${write.id}`,
		`status: ${write.status}`,
		`verdict: ${verdictLabel(write)}`,
	];
	if (write.status === "executed" && execution) {
		if (write.decided_by_name) lines.push(`approved_by: ${write.decided_by_name}`);
		if (write.trusted) lines.push("trusted: the workspace now runs this command without approval");
		lines.push("", renderExecution(execution));
		return lines.join("\n");
	}
	lines.push(`web_url: ${write.web_url}`);
	if (write.status === "required_approval") {
		lines.push(
			"",
			"This write is waiting for a human to approve it. Tell the user to open the link above and decide,",
			`then call ${CT_CLOUD_WRITE} again with only this write_id to run it.`,
		);
	} else if (write.status === "approved") {
		lines.push("", `Approved. Call ${CT_CLOUD_WRITE} again with only this write_id to run it.`);
	} else if (write.status === "declined") {
		lines.push(
			"",
			`Declined${write.decided_by_name ? ` by ${write.decided_by_name}` : ""}${write.decline_reason ? `: ${write.decline_reason}` : "."}`,
			"Do not retry this write. Tell the user.",
		);
	} else if (write.status === "denied") {
		lines.push(
			"",
			`Blocked by the workspace policy (${write.verdict}: ${write.verdict_reason}). It cannot be approved. Tell the user.`,
		);
	} else if (write.status === "failed") {
		lines.push(
			"",
			"The CloudThinker Sandbox could not start this write, so nothing ran. Tell the user; a retry is a new write.",
		);
	}
	return lines.join("\n");
}

export function writeSummary(
	outcome: WriteOutcome & Partial<Elapsed>,
	theme: Theme,
): string {
	const { write } = outcome;
	const verdict = verdictLabel(write);
	if (write.status === "executed") {
		const by = write.decided_by_name ? `${decidedBy(write)} · ` : "";
		return `${verdict} · ${by}ran in CloudThinker Sandbox · ${formatElapsed(outcome.elapsed_ms)}`;
	}
	if (write.status === "required_approval") {
		return `${verdict} · waiting for approval in browser → ${link(theme, write.web_url)}`;
	}
	if (write.status === "approved") return `${verdict} · approved, not run yet`;
	if (write.status === "failed") return `${verdict} · the Sandbox could not start it`;
	if (write.status === "declined") {
		return `${verdict} · declined${write.decided_by_name ? ` by ${write.decided_by_name}` : ""}`;
	}
	return `${verdict} · ${write.verdict_reason}`;
}

function missing(name: string): never {
	throw new Error(`${name} is required unless write_id resumes an earlier write.`);
}

interface WriteCall {
	runtime: CloudThinkerRuntime;
	ctx: ExtensionContext;
	params: Params;
	toolCallId: string;
	timeout: number;
	signal: AbortSignal | undefined;
	onUpdate: ((result: AgentToolResult<WriteOutcome & Elapsed>) => void) | undefined;
}

async function resolveOutcome(call: WriteCall): Promise<WriteOutcome> {
	const { runtime, params, signal } = call;
	if (params.write_id) {
		return { write: await runtime.client.getWrite(params.write_id, signal), execution: null };
	}
	return runtime.client.requestWrite(
		{
			conversation_id: runtime.requireSession().conversation_id,
			tool_call_id: call.toolCallId,
			connection_list: params.connection_list ?? [],
			script: params.script ?? missing("script"),
			reasoning: params.reasoning ?? missing("reasoning"),
			timeout: call.timeout,
			run_in_background: params.run_in_background ?? false,
			recent_user_messages: recentUserMessages(call.ctx.sessionManager),
		},
		signal,
	);
}

async function awaitApprovalIfNeeded(call: WriteCall, outcome: WriteOutcome): Promise<WriteOutcome> {
	const { runtime, ctx, signal } = call;
	if (outcome.write.status !== "required_approval") return outcome;
	const decided = await decideInTerminal(
		runtime.client,
		ctx,
		outcome.write,
		(message) => runtime.notify(message, "warning"),
		signal,
	);
	if (decided) return { write: decided, execution: null };
	runtime.awaitApproval(outcome.write.id, outcome.write.web_url, WRITE_SUBJECT, "");
	if (ctx.hasUI) ctx.ui.setWorkingMessage(WRITE_WORKING_MESSAGE);
	else process.stderr.write(`waiting for approval in the browser → ${outcome.write.web_url}\n`);
	const settled = await pollWrite({
		client: runtime.client,
		writeId: outcome.write.id,
		signal,
		onTick: (write, elapsedMs) =>
			call.onUpdate?.(
				text(`Waiting for approval (${Math.round(elapsedMs / 1000)}s).`, {
					write,
					execution: null,
					elapsed_ms: elapsedMs,
				}),
			),
	});
	return { write: settled, execution: null };
}

async function runIfApproved(call: WriteCall, outcome: WriteOutcome): Promise<WriteOutcome> {
	const { runtime, ctx, signal } = call;
	if (outcome.write.status !== "required_approval") runtime.clearApproval();
	if (outcome.write.status !== "approved") return outcome;
	if (ctx.hasUI) ctx.ui.setWorkingMessage("Running in CloudThinker Sandbox…");
	return runtime.client.runWrite(outcome.write.id, call.timeout, signal);
}

export function registerCloudWrite(runtime: CloudThinkerRuntime): void {
	runtime.pi.registerTool<typeof parameters, WriteOutcome & Elapsed>({
		name: CT_CLOUD_WRITE,
		label: "Cloud write",
		description,
		promptSnippet:
			"Run one state-changing command in CloudThinker's cloud once the workspace approves it",
		promptGuidelines: [
			`Route every state-changing cloud command through ${CT_CLOUD_WRITE}, one command per call. A human may have to approve it, in the terminal or in the browser, before it runs.`,
		],
		parameters,
		execute: async (
			toolCallId: string,
			params: Params,
			signal: AbortSignal | undefined,
			onUpdate,
			ctx,
		) => {
			runtime.requireSession();
			const startedAt = Date.now();
			const call: WriteCall = {
				runtime,
				ctx,
				params,
				toolCallId,
				timeout: params.timeout ?? DEFAULT_TIMEOUT_SECONDS,
				signal,
				onUpdate,
			};
			try {
				const requested = await resolveOutcome(call);
				const decided = await awaitApprovalIfNeeded(call, requested);
				const outcome = await runIfApproved(call, decided);
				runtime.noteWriteVerdict(outcome.write.verdict_reason);
				return text(renderWrite(outcome), { ...outcome, elapsed_ms: Date.now() - startedAt });
			} catch (error) {
				throw new Error(explain(error, params.connection_list ?? [], runtime));
			} finally {
				if (ctx.hasUI) ctx.ui.setWorkingMessage();
			}
		},
		renderCall: (params: Params, theme) =>
			callComponent(
				callLine(
					theme,
					CT_CLOUD_WRITE,
					params.connection_list?.join(", ") ?? "",
					params.write_id ? `resume ${params.write_id}` : firstLine(params.script ?? ""),
				),
			),
		renderResult: (result, options, theme) =>
			summaryComponent(
				theme,
				result.details ? writeSummary(result.details, theme) : "failed",
				resultBody(result),
				options.expanded,
			),
	});
}
