import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import type { Extension, ExtensionContext, LoadExtensionsResult, ToolDefinition } from "@earendil-works/pi-coding-agent";
import { resolveResumeTarget } from "@tintinweb/pi-subagents/dist/workflow/task.js";
import { fullWorkflowToolDescription } from "@tintinweb/pi-subagents/dist/workflow/tool-description.js";

import { CLOUD_ENTRY_TYPE } from "@cloudthinker/pi/src/runtime.ts";
import { childLoaderOptions, cloudDelegationTool, cloudEnabled, resolveCloudMode } from "../src/subagents.ts";

const modes = [
	{ provider: "cloudthinker", id: "light", name: "Light" },
	{ provider: "cloudthinker", id: "pro", name: "Pro" },
	{ provider: "anthropic", id: "claude-opus", name: "Opus" },
];
const registry = {
	getAll: () => modes,
	getAvailable: () => modes,
	find: (provider: string, id: string) => modes.find((mode) => mode.provider === provider && mode.id === id),
};

test("CA-SUB-3: child mode resolves only exact advertised CloudThinker modes", () => {
	assert.equal(resolveCloudMode("light", registry), modes[0]);
	assert.equal(resolveCloudMode("cloudthinker/pro", registry), modes[1]);
});

test("CA-SUB-4: vendor credentials and fuzzy names cannot escape CloudThinker modes", () => {
	for (const name of ["anthropic/claude-opus", "opus", "cl", "cloudthinker/removed", ""]) {
		assert.match(String(resolveCloudMode(name, registry)), /CloudThinker agent mode/);
	}
});

test("CA-SUB-5: empty catalog does not select a vendor model", () => {
	assert.match(String(resolveCloudMode("pro", { ...registry, getAll: () => [modes[2]!] })), /CloudThinker agent mode/);
});

test("CA-SUB-DESCRIPTION: upstream description drift fails closed", () => {
	assert.throws(() => cloudDelegationTool({ name: "Agent", description: "Unrecognized upstream schema", parameters: { type: "object", properties: {} } } as unknown as ToolDefinition), /Unsupported upstream Agent description/);
});

test("workflow tool description renders the effective session concurrency limit", () => {
	const previous = process.env.CLOUDTHINKER_WORKFLOW_MAX_CONCURRENCY;
	process.env.CLOUDTHINKER_WORKFLOW_MAX_CONCURRENCY = "4";
	const tool = {
		name: "ct_workflow",
		description: "Concurrent agent() calls are capped at the configured session limit; excess calls queue. Nested workflows share this limit. effort?: string, opts.effort overrides these settings up to opts.isolation: agentType, model, effort, isolation",
		parameters: { type: "object", properties: { args: { type: "object" } } },
		execute: async () => ({ content: [] }),
	} as unknown as ToolDefinition;
	try {
		cloudDelegationTool(tool);
		assert.match(tool.description, /calls are capped at 4; excess calls queue/);
		assert.match(tool.description, /Nested workflows share this limit/);
	} finally {
		if (previous === undefined) delete process.env.CLOUDTHINKER_WORKFLOW_MAX_CONCURRENCY;
		else process.env.CLOUDTHINKER_WORKFLOW_MAX_CONCURRENCY = previous;
	}
});

test("workflow description does not advertise stale runtime limits or resume constraints", () => {
	assert.doesNotMatch(fullWorkflowToolDescription, /min\(4, available CPUs - 2\)|only up to 4 run at any moment/);
	assert.doesNotMatch(fullWorkflowToolDescription, /the run must have finished/);
});

test("workflow resume recovers a persisted run after the in-memory task map is lost", () => {
	const root = mkdtempSync(join(tmpdir(), "ct-workflow-restart-"));
	const runId = "wf_123456789abc";
	const journalPath = join(root, `${runId}.workflow.jsonl`);
	const scriptPath = join(root, `${runId}.workflow.js`);
	writeFileSync(journalPath, '{"index":0,"key":"key","ok":true,"text":"done"}\n');
	writeFileSync(scriptPath, "export const meta = { name: 'saved', description: 'saved' }; return 'done';");
	try {
		assert.deepEqual(resolveResumeTarget(runId, new Map(), root), { ok: true, runId, journalPath, scriptPath });
		const missing = resolveResumeTarget("wf_123456789abd", new Map(), root);
		assert.equal(missing?.ok, false);
		if (missing?.ok === false) assert.match(missing.message, /No workflow run/);
		const traversal = resolveResumeTarget("wf_../../outside", new Map(), root);
		assert.equal(traversal?.ok, false);
		const directoryRunId = "wf_987654321abc";
		writeFileSync(join(root, `${directoryRunId}.workflow.jsonl`), "");
		mkdirSync(join(root, `${directoryRunId}.workflow.js`));
		const directory = resolveResumeTarget(directoryRunId, new Map(), root);
		assert.equal(directory?.ok, false);
	} finally {
		rmSync(root, { recursive: true, force: true });
	}
});

test("CA-SUB-8: a child of a setting-off parent gets no cloud tools, and an entry still wins", () => {
	const root = mkdtempSync(join(tmpdir(), "ct-cloud-default-"));
	const previous = process.env.PI_CODING_AGENT_DIR;
	process.env.PI_CODING_AGENT_DIR = join(root, "agent");
	mkdirSync(join(root, ".pi"), { recursive: true });
	writeFileSync(join(root, ".pi", "settings.json"), JSON.stringify({ cloudDefault: false }));
	try {
		const entries: unknown[] = [];
		const ctx = { cwd: root, sessionManager: { getEntries: () => entries }, isProjectTrusted: () => true } as unknown as ExtensionContext;
		const untrusted = { cwd: root, sessionManager: { getEntries: () => entries }, isProjectTrusted: () => false } as unknown as ExtensionContext;
		const load = () => childLoaderOptions({ cwd: root, agentDir: root, noExtensions: true }, ctx);
		const base = (cloud: Extension): LoadExtensionsResult => ({ extensions: [cloud], errors: [], runtime: {} } as unknown as LoadExtensionsResult);

		assert.equal(cloudEnabled(ctx), false);
		assert.equal(cloudEnabled(untrusted), true);
		const off = { path: "<inline:cloudthinker>", tools: new Map([["ct_ask", {}]]) } as unknown as Extension;
		load().extensionsOverride?.(base(off));
		assert.equal(off.tools.size, 0);

		entries.push({ type: "custom", customType: CLOUD_ENTRY_TYPE, data: { enabled: true } });
		assert.equal(cloudEnabled(ctx), true);
		assert.equal(cloudEnabled(untrusted), true);
		const on = { path: "<inline:cloudthinker>", tools: new Map([["ct_ask", {}]]) } as unknown as Extension;
		load().extensionsOverride?.(base(on));
		assert.equal(on.tools.size, 1);
	} finally {
		if (previous === undefined) delete process.env.PI_CODING_AGENT_DIR;
		else process.env.PI_CODING_AGENT_DIR = previous;
		rmSync(root, { recursive: true, force: true });
	}
});

test("running agents and commands share one Tasks pane, agents first", async () => {
	const { AgentWidget } = await import("@tintinweb/pi-subagents/dist/ui/agent-widget.js");
	const { setSubagentHost } = await import("@tintinweb/pi-subagents/dist/host.js");
	const { subagentHost } = await import("../src/subagents.ts");
	const { tasksPane } = await import("../src/tasks-pane.ts");
	const widgets = new Map<string, unknown>();
	const ui = {
		setWidget: (key: string, value: unknown) => { value === undefined ? widgets.delete(key) : widgets.set(key, value); },
		setStatus() {},
	};
	const theme = { fg: (_color: string, value: string) => value, bold: (value: string) => value };
	const render = () => {
		const factory = widgets.get("ct-tasks") as ((tui: unknown, theme: unknown) => { render(): string[] }) | undefined;
		return factory?.({ terminal: { columns: 160 }, requestRender() {} }, theme).render() ?? [];
	};
	const agents = [{ id: "agent-1", type: "Explore", status: "running", description: "find render path", toolUses: 0, startedAt: Date.now() }];
	setSubagentHost(subagentHost);
	tasksPane.bind(ui as never);
	const widget = new AgentWidget({ listAgents: () => agents } as never, new Map());
	widget.setUICtx(ui as never);
	widget.update();
	tasksPane.setGroup("Commands", () => ["└─ ⠋ sleep 30 · 1s"], 1);
	const lines = render();
	assert.equal(lines[0], "● Tasks");
	assert.equal(lines[1], "▾ Agents 1");
	assert.match(lines[2]!, /^└─ \S .+  find render path · /);
	assert.ok(lines.indexOf("▾ Commands 1") > 2);
	assert.equal(widgets.has("agents"), false);
	agents.length = 0;
	widget.update();
	tasksPane.setGroup("Commands", undefined);
	assert.equal(widgets.has("ct-tasks"), false);
	widget.dispose();
});
