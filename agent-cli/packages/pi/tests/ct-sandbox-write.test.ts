import assert from "node:assert/strict";
import test from "node:test";
import { stripVTControlCharacters } from "node:util";

import { initTheme } from "@earendil-works/pi-coding-agent";
import type { ExtensionAPI, ExtensionContext, ToolDefinition } from "@earendil-works/pi-coding-agent";

import type { CloudThinkerClient, CloudWrite, WriteOutcome, WriteRequest } from "../src/client.ts";
import { CloudThinkerApiError } from "../src/client.ts";
import { CloudThinkerRuntime } from "../src/runtime.ts";
import {
	APPROVE_AND_TRUST,
	APPROVE_HERE,
	DECIDE_IN_BROWSER,
	DECISION_OPTIONS,
	DECLINE_HERE,
	NOT_AN_APPROVER_MESSAGE,
	TRUST_DECISION_OPTIONS,
	TRUST_REFUSED_MESSAGE,
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
		trusted: false,
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

test("a trusted approval says so in the summary and tells the model the command is now trusted", () => {
	const trusted = write("executed", { decided_by_name: "Duc Bui", trusted: true, return_code: 0 });
	assert.equal(
		writeSummary({ write: trusted, execution: null, elapsed_ms: 1500 }, theme),
		"auto: needs approval · approved and trusted by Duc Bui · ran on the workspace machine · 1.5s",
	);
	const body = renderWrite({
		write: trusted,
		execution: { status: "completed", return_code: 0, stdout: "ok", stderr: "" },
	});
	assert.match(body, /approved_by: Duc Bui\ntrusted: the workspace now runs this command without approval/);
});

test("every verdict renders as the mode that decided it, with the trusted reason humanised", () => {
	assert.equal(verdictLabel({ verdict: "allow", verdict_reason: "workspace_trusted_command" }), "auto: allowed (trusted command)");
	assert.equal(verdictLabel({ verdict: "allow", verdict_reason: "classifier_allow" }), "auto: allowed (classifier_allow)");
	assert.equal(verdictLabel({ verdict: "require_approval", verdict_reason: "classifier" }), "auto: needs approval");
	assert.equal(verdictLabel({ verdict: "require_approval", verdict_reason: "auto_mode_disabled" }), "manual: needs approval");
	assert.equal(verdictLabel({ verdict: "escalate", verdict_reason: "escalation_pending" }), "auto: escalated");
	assert.equal(verdictLabel({ verdict: "hard_deny", verdict_reason: "destructive_policy" }), "auto: denied");

	const allowed = write("executed", { verdict: "allow", verdict_reason: "workspace_trusted_command" });
	assert.equal(
		writeSummary({ write: allowed, execution: null, elapsed_ms: 2000 }, theme),
		"auto: allowed (trusted command) · ran on the workspace machine · 2.0s",
	);
	assert.match(renderWrite({ write: allowed, execution: { status: "completed", return_code: 0, stdout: "", stderr: "" } }), /verdict: auto: allowed \(trusted command\)/);
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
		"⏸ A cloud write is waiting for your approval",
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

test("the terminal offers approve-and-trust as a fourth option, and only there", async () => {
	const trust = ui(APPROVE_AND_TRUST);
	assert.deepEqual(await askInTerminal(trust.ctx, write("required_approval")), {
		decision: "approve",
		trust: true,
	});
	assert.deepEqual(trust.offered, [TRUST_DECISION_OPTIONS]);
	assert.deepEqual(TRUST_DECISION_OPTIONS, [APPROVE_HERE, APPROVE_AND_TRUST, DECLINE_HERE, DECIDE_IN_BROWSER]);

	const plain = ui(APPROVE_HERE);
	await askInTerminal(plain.ctx, write("required_approval"), false);
	assert.deepEqual(plain.offered, [DECISION_OPTIONS]);
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

test("approve-and-trust sends trust:true and the trusted write comes back", async () => {
	const client = decider([write("approved", { trusted: true })]);
	const terminal = ui(APPROVE_AND_TRUST);
	const warnings: string[] = [];
	const decided = await decideInTerminal(client, terminal.ctx, write("required_approval"), (m) => warnings.push(m));
	assert.equal(decided?.trusted, true);
	assert.deepEqual(client.bodies, [{ decision: "approve", reason: undefined, trust: true }]);
	assert.deepEqual(warnings, []);
});

test("a refused trust applies nothing and asks again without the trust option", async () => {
	const client = decider([new CloudThinkerApiError(403, "cannot trust"), write("approved")]);
	const terminal = ui(APPROVE_AND_TRUST, undefined, APPROVE_HERE);
	const warnings: string[] = [];
	const decided = await decideInTerminal(client, terminal.ctx, write("required_approval"), (m) => warnings.push(m));
	assert.equal(decided?.status, "approved");
	assert.deepEqual(terminal.offered, [TRUST_DECISION_OPTIONS, DECISION_OPTIONS]);
	assert.deepEqual(client.bodies, [
		{ decision: "approve", reason: undefined, trust: true },
		{ decision: "approve", reason: undefined, trust: undefined },
	]);
	assert.deepEqual(warnings, [TRUST_REFUSED_MESSAGE]);
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
	} as unknown as ExtensionContext;
	return { runtime, tool, ctx };
}

const executed: WriteOutcome = {
	write: write("executed", { verdict: "allow", verdict_reason: "workspace_trusted_command", return_code: 0 }),
	execution: { status: "completed", return_code: 0, stdout: "tagged", stderr: "" },
};

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
	const failed = write("failed", { verdict: "allow", verdict_reason: "workspace_trusted_command" });

	const body = renderWrite({ write: failed, execution: null });
	assert.match(body, /status: failed/);
	assert.match(body, /could not start this write, so nothing ran/);
	assert.doesNotMatch(body, /again with only this write_id/);
	assert.equal(
		writeSummary({ write: failed, execution: null }, theme),
		"auto: allowed (trusted command) · the workspace machine could not start it",
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
