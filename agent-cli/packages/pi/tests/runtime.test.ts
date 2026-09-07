import assert from "node:assert/strict";
import test from "node:test";

import type {
	ExtensionAPI,
	ExtensionContext,
	ToolDefinition,
} from "@earendil-works/pi-coding-agent";

import type { CloudThinkerClient, RunState } from "../src/client.ts";
import { formatMirrorStatus } from "../src/mirror.ts";
import { APPROVAL_KEY, CloudThinkerRuntime, approvalWidgetLine } from "../src/runtime.ts";
import { registerRunStatus } from "../src/tools/ct-run-status.ts";
import { CT_RUN_STATUS } from "../src/tools/names.ts";
import { hostVersionsFrom } from "../src/versions.ts";

interface WidgetCall {
	key: string;
	content: string[] | undefined;
}

function runtimeWithWidgets(): { runtime: CloudThinkerRuntime; widgets: WidgetCall[] } {
	const widgets: WidgetCall[] = [];
	const runtime = new CloudThinkerRuntime(
		{} as ExtensionAPI,
		undefined,
		hostVersionsFrom({ version: "0.4.0", piVersion: "0.85.1" }, "0.4.0"),
	);
	runtime.bind({
		ui: {
			setWidget: (key: string, content: string[] | undefined) => widgets.push({ key, content }),
			setStatus: () => {},
			setTitle: () => {},
		},
	} as unknown as ExtensionContext);
	return { runtime, widgets };
}

test("an approval opens one widget above the editor and clearing removes it", () => {
	const { runtime, widgets } = runtimeWithWidgets();
	runtime.awaitApproval("r-1", "https://app.cloudthinker.io/chat/a-1");
	assert.equal(runtime.approvalRunId, "r-1");
	assert.deepEqual(widgets.at(-1), {
		key: APPROVAL_KEY,
		content: [approvalWidgetLine("https://app.cloudthinker.io/chat/a-1")],
	});

	runtime.clearApproval();
	assert.equal(runtime.approvalRunId, undefined);
	assert.deepEqual(widgets.at(-1), { key: APPROVAL_KEY, content: undefined });
});

function runStatusHarness(state: RunState): {
	runtime: CloudThinkerRuntime;
	widgets: WidgetCall[];
	read: () => Promise<unknown>;
} {
	const widgets: WidgetCall[] = [];
	const tools = new Map<string, ToolDefinition>();
	const runtime = new CloudThinkerRuntime(
		{
			registerTool: (tool: ToolDefinition) => tools.set(tool.name, tool),
		} as unknown as ExtensionAPI,
		{ getRun: async () => state } as unknown as CloudThinkerClient,
		hostVersionsFrom({ version: "0.4.0", piVersion: "0.85.1" }, "0.4.0"),
	);
	const ctx = {
		hasUI: false,
		ui: {
			setWidget: (key: string, content: string[] | undefined) => widgets.push({ key, content }),
			setStatus: () => {},
			setWorkingMessage: () => {},
		},
	} as unknown as ExtensionContext;
	runtime.bind(ctx);
	runtime.session = {
		conversation_id: "c-1",
		workspace_id: "w-1",
		web_url: "https://web/c-1",
		auto_mode: { enabled: false, can_edit: false },
	};
	registerRunStatus(runtime);
	const tool = tools.get(CT_RUN_STATUS);
	assert.ok(tool);
	return {
		runtime,
		widgets,
		read: () => tool.execute("call-1", { run_id: state.run_id }, undefined, undefined, ctx),
	};
}

function runState(status: RunState["status"], runId = "r-1"): RunState {
	return {
		run_id: runId,
		conversation_id: "h-1",
		status,
		answer: null,
		message: null,
		failure_kind: null,
		web_url: "https://web/a-1",
	};
}

test("a run that left the approval state takes its widget down", async () => {
	const harness = runStatusHarness(runState("succeeded"));
	harness.runtime.awaitApproval("r-1", "https://web/a-1");
	await harness.read();
	assert.equal(harness.runtime.approvalRunId, undefined);
	assert.deepEqual(harness.widgets.at(-1), { key: APPROVAL_KEY, content: undefined });
});

test("a status read for another run leaves the waiting widget alone", async () => {
	const harness = runStatusHarness(runState("succeeded", "r-2"));
	harness.runtime.awaitApproval("r-1", "https://web/a-1");
	await harness.read();
	assert.equal(harness.runtime.approvalRunId, "r-1");
	assert.deepEqual(harness.widgets.at(-1), {
		key: APPROVAL_KEY,
		content: [approvalWidgetLine("https://web/a-1")],
	});
});

test("the approval line names the browser, the run's own link, and the notify command; a write's line has no notify", () => {
	assert.equal(
		approvalWidgetLine("https://web/a-1"),
		"⏸ Anna is waiting for your approval in the browser → https://web/a-1 · /cloudthinker notify tells the approvers",
	);
	assert.equal(
		approvalWidgetLine("https://web/c-1", "A cloud write", ""),
		"⏸ A cloud write is waiting for your approval in the browser → https://web/c-1",
	);
});

test("a healthy mirror shows nothing and only a fault takes the footer", () => {
	assert.equal(
		formatMirrorStatus({ pending: 0, truncated: 0, rejected: 0, retryingAfterFailure: false }),
		undefined,
	);
	assert.equal(
		formatMirrorStatus({ pending: 3, truncated: 0, rejected: 0, retryingAfterFailure: false }),
		undefined,
	);
	assert.equal(
		formatMirrorStatus({ pending: 3, truncated: 1, rejected: 2, retryingAfterFailure: false }),
		"1 truncated, 2 rejected",
	);
	assert.equal(
		formatMirrorStatus({ pending: 7, truncated: 0, rejected: 0, retryingAfterFailure: true }),
		"✕ mirror offline, 7 pending",
	);
});
