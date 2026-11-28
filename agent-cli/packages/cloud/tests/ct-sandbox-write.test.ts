import assert from "node:assert/strict";
import { PassThrough } from "node:stream";
import test from "node:test";
import { stripVTControlCharacters } from "node:util";

import { initTheme } from "@earendil-works/pi-coding-agent";
import type { ExtensionAPI, ExtensionContext, ExtensionToolContext, ToolDefinition } from "@earendil-works/pi-coding-agent";

import type { CloudThinkerClient, CloudWrite, WriteOutcome, WriteRequest } from "../src/client.ts";
import { CloudThinkerApiError } from "../src/client.ts";
import { CloudThinkerRuntime } from "../src/runtime.ts";
import {
	APPROVE_HERE,
	DECIDE_IN_BROWSER,
	DECISION_OPTIONS,
	DECLINE_HERE,
	NOT_AN_APPROVER_MESSAGE,
	approvalCard,
	askInTerminal,
	decideInTerminal,
	pollWrite,
	recentUserMessages,
	registerSandboxWrite,
	renderWrite,
	verdictLabel,
	writeSummary,
} from "../src/tools/ct-sandbox-write.ts";
import { CT_SANDBOX_WRITE } from "../src/tools/names.ts";
import { resultBody } from "../src/tools/render.ts";
import { hostVersionsFrom } from "../src/versions.ts";

function write(status: CloudWrite["status"], extra: Partial<CloudWrite> = {}): CloudWrite {
	return {
		id: "w-1",
		conversation_id: "c-1",
		tool_call_id: "call_1",
		connection_list: ["aws"],
		script: "aws ec2 create-tags --resources i-1 --tags Key=owner,Value=duc",
		reasoning: "Tag the orphaned instance with its owner.",
		verdict: "require_approval",
		verdict_reason: "classifier",
		status,
		decided_by_name: null,
		decline_reason: null,
		task_id: null,
		return_code: null,
		expires_at: "2026-09-07T12:00:00Z",
		web_url: "http://web/chat?conversationId=c-1",
		...extra,
	};
}

function reader(states: CloudWrite[]): { getWrite(): Promise<CloudWrite>; calls: number } {
	const box = {
		calls: 0,
		getWrite: async (): Promise<CloudWrite> => {
			const next = states[Math.min(box.calls, states.length - 1)];
			box.calls += 1;
			return next as CloudWrite;
		},
	};
	return box;
}

const theme = { fg: (_color: string, value: string) => value, bold: (value: string) => value } as never;

test("polling stops on the first state that is no longer waiting", async () => {
	const client = reader([write("required_approval"), write("required_approval"), write("approved")]);
	const ticks: string[] = [];
	const settled = await pollWrite({
		client,
		writeId: "w-1",
		intervalMs: 0,
		onTick: (state) => ticks.push(state.status),
	});
	assert.equal(settled.status, "approved");
	assert.deepEqual(ticks, ["required_approval", "required_approval", "approved"]);
	assert.equal(client.calls, 3);
});

test("polling gives up at the deadline and returns the waiting state", async () => {
	let clock = 0;
	const client = reader([write("required_approval")]);
	const settled = await pollWrite({
		client,
		writeId: "w-1",
		intervalMs: 0,
		maxWaitMs: 10,
		now: () => (clock += 3),
	});
	assert.equal(settled.status, "required_approval");
	assert.equal(client.calls, 3);
});

test("the classifier sees the developer's last three prompts, text blocks flattened", () => {
	const branch = [
		{ type: "message", message: { role: "user", content: "one" } },
		{ type: "message", message: { role: "assistant", content: "reply" } },
		{ type: "message", message: { role: "user", content: [{ type: "text", text: "two" }] } },
		{ type: "message", message: { role: "toolResult", content: "ignored" } },
		{ type: "message", message: { role: "user", content: "three" } },
		{ type: "message", message: { role: "user", content: "four" } },
	];
	const messages = recentUserMessages({ getBranch: () => branch as never });
	assert.deepEqual(messages, ["two", "three", "four"]);
});

test("an executed write renders the approver and the sandbox output", () => {
	const body = renderWrite({
		write: write("executed", { decided_by_name: "Duc Bui", return_code: 0 }),
		execution: { status: "completed", return_code: 0, stdout: "ok", stderr: "" },
	});
	assert.match(body, /status: executed/);
	assert.match(body, /approved_by: Duc Bui/);
	assert.ok(body.endsWith("\n\nok"), body);
	assert.equal(
		writeSummary({ write: write("executed", { decided_by_name: "Duc Bui" }), execution: null, elapsed_ms: 1500 }, theme),
		"auto: needs approval · approved by Duc Bui · ran on the workspace machine · 1.5s",
	);
});

test("every verdict renders as the mode that decided it", () => {
	assert.equal(verdictLabel({ verdict: "allow", verdict_reason: "classifier_allow" }), "auto: allowed (classifier_allow)");
	assert.equal(verdictLabel({ verdict: "require_approval", verdict_reason: "classifier" }), "auto: needs approval");
	assert.equal(verdictLabel({ verdict: "require_approval", verdict_reason: "auto_mode_disabled" }), "manual: needs approval");
	assert.equal(verdictLabel({ verdict: "escalate", verdict_reason: "escalation_pending" }), "auto: escalated");
	assert.equal(verdictLabel({ verdict: "hard_deny", verdict_reason: "destructive_policy" }), "auto: denied");

	const allowed = write("executed", { verdict: "allow", verdict_reason: "classifier_allow" });
	assert.equal(
		writeSummary({ write: allowed, execution: null, elapsed_ms: 2000 }, theme),
		"auto: allowed (classifier_allow) · ran on the workspace machine · 2.0s",
	);
	assert.match(renderWrite({ write: allowed, execution: { status: "completed", return_code: 0, stdout: "", stderr: "" } }), /verdict: auto: allowed \(classifier_allow\)/);
	assert.match(
		writeSummary({ write: write("required_approval", { verdict_reason: "auto_mode_disabled" }), execution: null }, theme),
		/^manual: needs approval · waiting for approval in browser → /,
	);
	assert.match(
		writeSummary({ write: write("required_approval", { verdict: "escalate", verdict_reason: "escalation_pending" }), execution: null }, theme),
		/^auto: escalated · waiting for approval in browser → /,
	);
	assert.equal(
		writeSummary({ write: write("declined", { verdict_reason: "auto_mode_disabled", decided_by_name: "Hao" }), execution: null }, theme),
		"manual: needs approval · declined by Hao",
	);
});

test("a waiting write tells the model to stop and resume by write_id", () => {
	const body = renderWrite({ write: write("required_approval"), execution: null });
	assert.match(body, /web_url: http:\/\/web/);
	assert.match(body, new RegExp(`call ${CT_SANDBOX_WRITE} again with only this write_id`));
	assert.match(
		writeSummary({ write: write("required_approval"), execution: null }, theme),
		/^auto: needs approval · waiting for approval in browser → /,
	);
});

test("a declined or denied write names the reason and forbids a retry", () => {
	const declined = renderWrite({
		write: write("declined", { decided_by_name: "Hao", decline_reason: "Wrong instance." }),
		execution: null,
	});
	assert.match(declined, /Declined by Hao: Wrong instance\./);
	assert.match(declined, /Do not retry/);
	const denied = renderWrite({
		write: write("denied", { verdict: "hard_deny", verdict_reason: "destructive_policy" }),
		execution: null,
	});
	assert.match(denied, /hard_deny: destructive_policy/);
	assert.equal(
		writeSummary({ write: write("denied", { verdict: "hard_deny", verdict_reason: "destructive_policy" }), execution: null }, theme),
		"auto: denied · destructive_policy",
	);
});

test("the terminal card carries the reasoning, the connection, the command, and the link", () => {
	assert.deepEqual(approvalCard(write("required_approval")), [
		"approval needed: A cloud write is waiting",
		"  Tag the orphaned instance with its owner.",
		"  cloud · aws: aws ec2 create-tags --resources i-1 --tags Key=owner,Value=duc",
		"  browser → http://web/chat?conversationId=c-1",
	]);
});

function ui(choice: string | undefined, reason?: string, ...laterChoices: (string | undefined)[]) {
	const widgets: unknown[] = [];
	const offered: string[][] = [];
	const choices = [choice, ...laterChoices];
	return {
		widgets,
		offered,
		ctx: {
			hasUI: true,
			ui: {
				setWidget: (_key: string, content: unknown) => widgets.push(content),
				select: async (_title: string, options: string[]) => {
					offered.push(options);
					return choices.length > 1 ? choices.shift() : choices[0];
				},
				input: async () => reason,
			},
		} as never,
	};
}

test("the developer decides in the terminal: approve, decline with a reason, escape declines, or hand off to the browser", async () => {
	const approve = ui(APPROVE_HERE);
	assert.deepEqual(await askInTerminal(approve.ctx, write("required_approval")), { decision: "approve" });
	assert.equal(approve.widgets.length, 1);

	const decline = ui(DECLINE_HERE, "  Wrong instance.  ");
	assert.deepEqual(await askInTerminal(decline.ctx, write("required_approval")), {
		decision: "decline",
		reason: "Wrong instance.",
	});

	const browser = ui(DECIDE_IN_BROWSER);
	assert.deepEqual(await askInTerminal(browser.ctx, write("required_approval")), { decision: "browser" });

	const escaped = ui(undefined);
	assert.deepEqual(await askInTerminal(escaped.ctx, write("required_approval")), { decision: "decline" });

	const headless = { hasUI: false, ui: {} } as never;
	assert.deepEqual(await askInTerminal(headless, write("required_approval")), { decision: "browser" });
});

function modal(...keys: string[]) {
	const screens: string[] = [];
	const widgets: unknown[] = [];
	return {
		screens,
		widgets,
		ctx: {
			hasUI: true,
			mode: "tui",
			ui: {
				setWidget: (_key: string, content: unknown) => widgets.push(content),
				select: async () => {
					throw new Error("the TUI must use the approval modal, not the plain selector");
				},
				input: async () => undefined,
				custom: async <T>(factory: (tui: unknown, theme: unknown, keybindings: unknown, done: (result: T) => void) => { render(width: number): string[]; handleInput(data: string): void }) => {
					let result: T | undefined;
					let finished = false;
					const component = factory({ requestRender: () => {}, terminal: { rows: 40 } }, theme, {}, (value) => {
						result = value;
						finished = true;
					});
					for (const key of keys) {
						if (finished) break;
						screens.push(component.render(80).join("\n"));
						component.handleInput(key);
					}
					return result;
				},
			},
		} as never,
	};
}

test("the TUI asks in one modal that shows the whole command; escape declines and nothing approves by accident", async () => {
	const multiline = write("required_approval", { script: "kubectl scale deploy/api --replicas=3\nkubectl rollout status deploy/api" });
	const quick = modal("y");
	assert.deepEqual(await askInTerminal(quick.ctx, multiline), { decision: "approve" });
	assert.deepEqual(quick.widgets, []);
	assert.match(quick.screens[0]!, /Cloud write needs your approval/);
	assert.match(quick.screens[0]!, /Tag the orphaned instance with its owner\./);
	assert.match(quick.screens[0]!, /cloud · aws/);
	assert.match(quick.screens[0]!, /kubectl rollout status deploy\/api/);
	assert.match(quick.screens[0]!, /1\. Approve and run\s+y/);
	assert.match(quick.screens[0]!, /esc decline/);

	assert.deepEqual(await askInTerminal(modal("\x1b").ctx, multiline), { decision: "decline" });
	assert.deepEqual(await askInTerminal(modal("b").ctx, multiline), { decision: "browser" });
	assert.deepEqual(await askInTerminal(modal("\x1b[B", "\r").ctx, multiline), { decision: "decline", reason: undefined });
	assert.deepEqual(await askInTerminal(modal("x", "3").ctx, multiline), { decision: "browser" });
	assert.deepEqual(await askInTerminal(modal("a").ctx, multiline), { decision: "decline" });
});

test("the terminal offers approve, decline, and the browser", async () => {
	const plain = ui(APPROVE_HERE);
	assert.deepEqual(await askInTerminal(plain.ctx, write("required_approval")), { decision: "approve" });
	assert.deepEqual(plain.offered, [DECISION_OPTIONS]);
	assert.deepEqual(DECISION_OPTIONS, [APPROVE_HERE, DECLINE_HERE, DECIDE_IN_BROWSER]);
});

function decider(outcomes: (CloudWrite | CloudThinkerApiError)[]) {
	const bodies: unknown[] = [];
	return {
		bodies,
		decideWrite: async (_id: string, body: unknown) => {
			bodies.push(body);
			const next = outcomes.shift();
			if (next instanceof CloudThinkerApiError) throw next;
			return next as CloudWrite;
		},
	};
}

test("an approve sends only the decision and the approved write comes back", async () => {
	const client = decider([write("approved")]);
	const terminal = ui(APPROVE_HERE);
	const warnings: string[] = [];
	const decided = await decideInTerminal(client, terminal.ctx, write("required_approval"), (m) => warnings.push(m));
	assert.equal(decided?.status, "approved");
	assert.deepEqual(client.bodies, [{ decision: "approve", reason: undefined }]);
	assert.deepEqual(warnings, []);
});

test("a plain approve that is refused hands the write to the browser", async () => {
	const client = decider([new CloudThinkerApiError(403, "not an approver")]);
	const terminal = ui(APPROVE_HERE);
	const warnings: string[] = [];
	const decided = await decideInTerminal(client, terminal.ctx, write("required_approval"), (m) => warnings.push(m));
	assert.equal(decided, undefined);
	assert.deepEqual(warnings, [NOT_AN_APPROVER_MESSAGE]);
	assert.equal(terminal.offered.length, 1);
});

function registered(client: Partial<CloudThinkerClient>) {
	const tools = new Map<string, ToolDefinition>();
	const runtime = new CloudThinkerRuntime(
		{
			registerTool: (tool: ToolDefinition) => tools.set(tool.name, tool),
			appendEntry: () => {},
		} as unknown as ExtensionAPI,
		client as CloudThinkerClient,
		hostVersionsFrom({ version: "0.4.0", piVersion: "0.85.1" }, "0.4.0"),
	);
	runtime.session = {
		conversation_id: "c-1",
		workspace_id: "w-1",
		web_url: "http://web/c-1",
		auto_mode: { enabled: true, can_edit: false },
	};
	runtime.connections = { xml: "", prefixes: ["aws"] };
	registerSandboxWrite(runtime);
	const tool = tools.get(CT_SANDBOX_WRITE);
	assert.ok(tool);
	const ctx = {
		hasUI: false,
		ui: { setWidget: () => {}, setWorkingMessage: () => {}, notify: () => {} },
		sessionManager: { getBranch: () => [] },
	} as unknown as ExtensionToolContext;
	return { runtime, tool, ctx };
}

const executed: WriteOutcome = {
	write: write("executed", { verdict: "allow", verdict_reason: "classifier_allow", return_code: 0 }),
	execution: { status: "completed", return_code: 0, stdout: "tagged", stderr: "" },
};

test("agent print mode sends a decision once and leaves browser approvals pending", async (t) => {
	const approverMessage = "Ask Alex to approve this write in the browser.";
	const cases = [
		{ input: "y\n", allowed: true, status: "executed", calls: ["request", "approve:w-1", "run:w-1"] },
		{ input: "n\n", allowed: true, status: "declined", calls: ["request", "decline:w-1"] },
		{ input: "\n", allowed: true, status: "required_approval", calls: ["request"] },
		{ input: "y\n", allowed: false, status: "required_approval", calls: ["request", "approve:w-1"] },
	] as const;
	const stdin = Object.assign(new PassThrough(), { isTTY: true });
	const stdout = new PassThrough();
	const stderr = Object.assign(new PassThrough(), { isTTY: true });
	t.mock.getter(process, "stdin", () => stdin as unknown as typeof process.stdin);
	t.mock.getter(process, "stdout", () => stdout as unknown as typeof process.stdout);
	t.mock.getter(process, "stderr", () => stderr as unknown as typeof process.stderr);
	const pendingWrite = write("required_approval", { script: "printf first\nprintf last" });
	let terminalText = "";
	stderr.on("data", (chunk: Buffer) => {
		terminalText += chunk.toString();
	});

	for (const scenario of cases) {
		const calls: string[] = [];
		const { tool, ctx } = registered({
			requestWrite: async () => {
				calls.push("request");
				return { write: pendingWrite, execution: null };
			},
			decideWrite: async (writeId, answer) => {
				calls.push(`${answer.decision}:${writeId}`);
				if (!scenario.allowed) {
					throw new CloudThinkerApiError(403, approverMessage, { code: "AGENT_CLI_APPROVER_REQUIRED" });
				}
				return write(answer.decision === "approve" ? "approved" : "declined");
			},
			getWrite: async () => {
				throw new Error("interactive print approval must not poll the browser");
			},
			runWrite: async (writeId) => {
				calls.push(`run:${writeId}`);
				return executed;
			},
		});
		// The terminal returns a line after showing the prompt.
		stderr.once("data", () => setImmediate(() => stdin.write(scenario.input)));
		const result = await tool.execute(
			"call-1",
			{ script: pendingWrite.script, reasoning: pendingWrite.reasoning },
			undefined,
			undefined,
			{ ...ctx, mode: "print" },
		);
		assert.equal((result.details as WriteOutcome).write.status, scenario.status);
		assert.deepEqual(calls, scenario.calls);
	}
	assert.match(terminalText, /printf first\n  printf last/);
	assert.match(terminalText, /Browser: http:\/\/web/);
	assert.match(terminalText, /Approve \[y\], decline \[n\]/);
	assert.match(terminalText, /Ask Alex to approve this write in the browser\. http:\/\/web/);
});

test("agent print mode points a requester to their approver without trying to decide", async () => {
	const pending = write("required_approval", {
		can_respond: false,
		approval_instructions: "Ask Alex to open the approval link in their browser.",
	});
	const { tool, ctx } = registered({
		requestWrite: async () => ({ write: pending, execution: null }),
		getWrite: async () => { throw new Error("print mode must not poll for approval"); },
		decideWrite: async () => { throw new Error("the requester cannot decide this write"); },
	});
	const result = await tool.execute(
		"call-1",
		{ script: pending.script, reasoning: pending.reasoning },
		undefined,
		undefined,
		{ ...ctx, mode: "print" },
	);
	assert.match(resultBody(result), /Ask Alex to open the approval link/);
	assert.match(resultBody(result), /write_id: w-1/);
});

test("the interactive agent runs the write when a workspace approver decides in the browser", async () => {
	const pending = write("required_approval", {
		can_respond: false,
		approval_instructions: "Ask Alex to open the approval link in their browser.",
	});
	const { tool, ctx } = registered({
		requestWrite: async () => ({ write: pending, execution: null }),
		getWrite: async () => write("approved", { decided_by_name: "Alex" }),
		decideWrite: async () => { throw new Error("the requester cannot decide this write"); },
		runWrite: async () => executed,
	});
	const result = await tool.execute(
		"call-1",
		{ script: pending.script, reasoning: pending.reasoning },
		undefined,
		undefined,
		{ ...ctx, mode: "tui", hasUI: true },
	);
	assert.equal((result.details as WriteOutcome).write.status, "executed");
	assert.match(resultBody(result), /tagged/);
});

test("agent print mode without a terminal returns the pending write immediately", async (t) => {
	const stdin = new PassThrough();
	t.mock.getter(process, "stdin", () => stdin as unknown as typeof process.stdin);
	const pending = write("required_approval", { can_respond: true });
	const { tool, ctx } = registered({
		requestWrite: async () => ({ write: pending, execution: null }),
		getWrite: async () => { throw new Error("print mode must not poll for approval"); },
	});
	const result = await tool.execute(
		"call-1",
		{ script: pending.script, reasoning: pending.reasoning },
		undefined,
		undefined,
		{ ...ctx, mode: "print" },
	);
	assert.equal((result.details as WriteOutcome).write.status, "required_approval");
	assert.match(resultBody(result), /web_url: http:\/\/web/);
});

test("a write the workspace allows runs on the first call and returns executed", async () => {
	const calls: string[] = [];
	const { tool, ctx } = registered({
		requestWrite: async (body: WriteRequest) => {
			calls.push(`request:${body.script}`);
			return executed;
		},
		getWrite: async () => {
			calls.push("get");
			return executed.write;
		},
		runWrite: async () => {
			calls.push("run");
			return executed;
		},
	});
	const result = await tool.execute(
		"call-1",
		{ connection_list: ["aws"], script: "aws ec2 create-tags", reasoning: "Tag it." },
		undefined,
		undefined,
		ctx,
	);
	const details = result.details as WriteOutcome;
	assert.equal(details.write.status, "executed");
	assert.match(resultBody(result), /status: executed[\s\S]*\n\ntagged$/);
	assert.deepEqual(calls, ["request:aws ec2 create-tags"]);
});

test("a resumed write_id reads the write and runs it once it is approved, re-sending no script", async () => {
	const calls: unknown[] = [];
	const { tool, ctx } = registered({
		requestWrite: async () => {
			calls.push("request");
			return executed;
		},
		getWrite: async (writeId: string) => {
			calls.push(`get:${writeId}`);
			return write("approved", { decided_by_name: "Hao" });
		},
		runWrite: async (writeId: string, timeoutSeconds: number) => {
			calls.push(`run:${writeId}:${timeoutSeconds}`);
			return { ...executed, write: { ...executed.write, decided_by_name: "Hao" } };
		},
	});
	const result = await tool.execute("call-2", { write_id: "w-1" }, undefined, undefined, ctx);
	const details = result.details as WriteOutcome;
	assert.equal(details.write.status, "executed");
	assert.match(resultBody(result), /approved_by: Hao/);
	assert.deepEqual(calls, ["get:w-1", "run:w-1:60"]);
});

test("a fresh write without a reasoning is refused before any request leaves", async () => {
	const calls: unknown[] = [];
	const { tool, ctx } = registered({
		requestWrite: async () => {
			calls.push("request");
			return executed;
		},
	});
	await assert.rejects(
		tool.execute("call-3", { connection_list: ["aws"], script: "aws ec2 create-tags" }, undefined, undefined, ctx),
		/reasoning is required unless write_id resumes an earlier write/,
	);
	assert.deepEqual(calls, []);
});

test("a 422 for an unconnected prefix names the requested and connected prefixes", async () => {
	const { tool, ctx } = registered({
		requestWrite: async () => {
			throw new CloudThinkerApiError(422, "Connections not available: k8s");
		},
	});
	await assert.rejects(
		() =>
			tool.execute(
				"call-3",
				{ connection_list: ["k8s"], script: "kubectl delete pod x", reasoning: "Restart it." },
				undefined,
				undefined,
				ctx,
			),
		(error: unknown) => {
			assert.ok(error instanceof Error);
			assert.match(error.message, /Connections not available: k8s/);
			assert.match(error.message, /Requested: k8s\./);
			assert.match(error.message, /Not connected in this workspace: k8s\./);
			assert.match(error.message, /Connected prefixes: aws\./);
			return true;
		},
	);
});

test("a write the workspace machine could not start says so and asks for no retry by id", () => {
	const failed = write("failed", { verdict: "allow", verdict_reason: "classifier_allow" });

	const body = renderWrite({ write: failed, execution: null });
	assert.match(body, /status: failed/);
	assert.match(body, /could not start this write, so nothing ran/);
	assert.doesNotMatch(body, /again with only this write_id/);
	assert.equal(
		writeSummary({ write: failed, execution: null }, theme),
		"auto: allowed (classifier_allow) · the workspace machine could not start it",
	);
});

test("an unconfirmed dispatch never claims nothing ran or invites a replay", () => {
	const outcome = { write: write("outcome_unknown"), execution: null };
	assert.match(renderWrite(outcome), /script may have started/);
	assert.match(renderWrite(outcome), /Do not replay this write automatically/);
	assert.doesNotMatch(renderWrite(outcome), /nothing ran/);
	assert.match(writeSummary(outcome, theme), /execution outcome unknown; do not replay/);
});

test("the call line shows the reasoning for the approver, and the command only when expanded", () => {
	const { tool } = registered({});
	assert.ok(tool.renderCall);
	const params = {
		connection_list: ["aws"],
		reasoning: "Tag the orphaned instance with its owner.",
		script: "aws ec2 create-tags --resources i-1 --tags Key=owner,Value=duc",
	};
	const [collapsed] = tool.renderCall(params, theme, { expanded: false } as never).render(200);
	assert.match(collapsed ?? "", /ct_sandbox_write {2}aws {2}Tag the orphaned instance with its owner\.$/);
	const expanded = tool.renderCall(params, theme, { expanded: true } as never).render(200);
	assert.deepEqual(expanded.slice(1).map((line) => line.trimEnd()), [params.script]);
	const [resumed] = tool.renderCall({ write_id: "w-1" }, theme, { expanded: true } as never).render(200);
	assert.match(resumed ?? "", /resume w-1$/);
});

test("a collapsed write result keeps the summary and hides the output behind the expand hint", () => {
	initTheme("dark");
	const { renderResult } = registered({}).tool;
	assert.ok(renderResult);
	const outcome = {
		write: write("executed"),
		execution: { status: "completed", return_code: 0, stdout: "tagged\ni-1", stderr: "" },
		elapsed_ms: 2000,
	};
	const render = (expanded: boolean) =>
		renderResult(
			{ content: [{ type: "text", text: "tagged\ni-1" }], details: outcome } as never,
			{ expanded, isPartial: false },
			theme,
			{ isError: false } as never,
		)
			.render(200)
			.map((line) => stripVTControlCharacters(line).trimEnd());
	const collapsed = render(false);
	assert.equal(collapsed.length, 2);
	assert.equal(collapsed[0], writeSummary(outcome as never, theme));
	assert.match(collapsed[1] ?? "", /^\(2 lines, .* to expand\)$/);
	assert.ok(render(true).includes("i-1"));
});
