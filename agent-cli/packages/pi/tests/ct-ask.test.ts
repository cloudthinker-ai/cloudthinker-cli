import assert from "node:assert/strict";
import test from "node:test";

import type { ExtensionAPI, ExtensionContext, ToolDefinition } from "@earendil-works/pi-coding-agent";

import type { CloudThinkerClient, RunState, RunSubmitted } from "../src/client.ts";
import { CloudThinkerRuntime, NOTIFY_HINT } from "../src/runtime.ts";
import { pollRun, registerAsk, renderRun, submitBody } from "../src/tools/ct-ask.ts";
import { CT_ASK, CT_RUN_STATUS } from "../src/tools/names.ts";
import { hostVersionsFrom } from "../src/versions.ts";

function state(status: RunState["status"], extra: Partial<RunState> = {}): RunState {
	return {
		run_id: "r-1",
		conversation_id: "h-1",
		status,
		answer: null,
		message: null,
		failure_kind: null,
		web_url: "http://web/runs/r-1",
		...extra,
	};
}

function reader(states: RunState[]): {
	getRun(): Promise<RunState>;
	calls: number;
} {
	const box = {
		calls: 0,
		getRun: async (): Promise<RunState> => {
			const next = states[Math.min(box.calls, states.length - 1)];
			box.calls += 1;
			return next as RunState;
		},
	};
	return box;
}

test("polling stops on the first succeeded state and reports every tick", async () => {
	const client = reader([state("pending"), state("running"), state("succeeded", {
		answer: "aws",
	})]);
	const ticks: string[] = [];
	const result = await pollRun({
		client,
		runId: "r-1",
		intervalMs: 0,
		onTick: (current) => ticks.push(current.status),
	});
	assert.equal(result.status, "succeeded");
	assert.deepEqual(ticks, ["pending", "running", "succeeded"]);
	assert.equal(client.calls, 3);
});

test("an approval pause returns at once instead of waiting out the cap", async () => {
	const client = reader([state("required_approval")]);
	const result = await pollRun({ client, runId: "r-1", intervalMs: 0 });
	assert.equal(result.status, "required_approval");
	assert.equal(client.calls, 1);
});

test("the client cap returns the last running state rather than hanging", async () => {
	const client = reader([state("running")]);
	let clock = 0;
	const result = await pollRun({
		client,
		runId: "r-1",
		intervalMs: 0,
		maxWaitMs: 10,
		now: () => {
			clock += 6;
			return clock;
		},
	});
	assert.equal(result.status, "running");
	assert.ok(client.calls <= 3);
});

test("a rendered run always names the run id, the status, and the pickup tool", () => {
	const succeeded = renderRun(state("succeeded", { answer: "eu-west-1" }));
	assert.ok(succeeded.includes("run_id: r-1"));
	assert.ok(succeeded.includes("eu-west-1"));

	const approval = renderRun(state("required_approval"));
	assert.ok(approval.includes("waiting for a human to approve this in the browser at http://web/runs/r-1"));
	assert.ok(approval.includes(NOTIFY_HINT));
	assert.ok(!approval.includes("notification was sent"));
	assert.ok(approval.includes(CT_RUN_STATUS));

	const running = renderRun(state("running"));
	assert.ok(running.includes(CT_RUN_STATUS));

	const failed = renderRun(state("failed", { failure_kind: "tool_error" }));
	assert.ok(failed.includes("failure_kind: tool_error"));
});

test("the first ask names the session as its source and later asks continue the thread", async () => {
	assert.deepEqual(submitBody("why", "c-1", undefined), { prompt: "why", source_conversation_id: "c-1" });
	assert.deepEqual(submitBody("more", "c-1", { conversation_id: "h-1" }), {
		prompt: "more",
		conversation_id: "h-1",
	});

	const bodies: unknown[] = [];
	const tools = new Map<string, ToolDefinition>();
	const entries: unknown[] = [];
	const client = {
		submitRun: async (body: unknown): Promise<RunSubmitted> => {
			bodies.push(body);
			return { run_id: "r-1", conversation_id: "h-1", status: "running", web_url: "http://web/h-1" };
		},
		getRun: async () => state("succeeded", { answer: "done" }),
	} as unknown as CloudThinkerClient;
	const runtime = new CloudThinkerRuntime(
		{
			registerTool: (tool: ToolDefinition) => tools.set(tool.name, tool),
			appendEntry: (type: string, data: unknown) => entries.push({ type, data }),
		} as unknown as ExtensionAPI,
		client,
		hostVersionsFrom({ version: "0.4.0", piVersion: "0.85.1" }, "0.4.0"),
	);
	runtime.session = {
		conversation_id: "c-1",
		workspace_id: "w-1",
		web_url: "http://web/c-1",
		auto_mode: { enabled: false, can_edit: false },
	};
	registerAsk(runtime);
	const tool = tools.get(CT_ASK);
	assert.ok(tool);
	const ctx = { hasUI: false, ui: { setWidget: () => {} } } as unknown as ExtensionContext;
	await tool.execute("call-1", { prompt: "why" }, undefined, undefined, ctx);
	await tool.execute("call-2", { prompt: "more" }, undefined, undefined, ctx);
	assert.deepEqual(bodies, [
		{ prompt: "why", source_conversation_id: "c-1" },
		{ prompt: "more", conversation_id: "h-1" },
	]);
	assert.equal(entries.length, 1);
});
