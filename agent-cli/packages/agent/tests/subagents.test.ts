import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import type { Extension, ExtensionContext, LoadExtensionsResult, ToolDefinition } from "@earendil-works/pi-coding-agent";
import { resolveResumeTarget } from "@cloudthinker/subagents/src/workflow/task.ts";
import { fullWorkflowToolDescription } from "@cloudthinker/subagents/src/workflow/tool-description.ts";

import { CLOUD_ENTRY_TYPE } from "@cloudthinker/cloud/src/runtime.ts";
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

test("in the terminal every Agent runs in the background, and its launch and a workflow card collapse to one row", async () => {
	const { Text } = await import("@earendil-works/pi-tui");
	const seen: unknown[] = [];
	const agent = cloudDelegationTool({
		name: "Agent",
		description: '- Use model to specify a different model (as "provider/modelId", or fuzzy e.g. "haiku", "sonnet").\n- Use thinking to control extended thinking level.\n',
		parameters: { type: "object", properties: { run_in_background: { type: "boolean" } } },
		execute: async (_id: string, params: unknown) => { seen.push(params); return { content: [] }; },
		renderResult: () => new Text("  ⎿  Running in background (ID: a1)", 0, 0),
	} as unknown as ToolDefinition);
	await agent.execute("call", { prompt: "p", run_in_background: false }, undefined, undefined, { mode: "tui" } as never);
	await agent.execute("call", { prompt: "p", run_in_background: false }, undefined, undefined, { mode: "print" } as never);
	assert.deepEqual(seen, [{ prompt: "p", run_in_background: true }, { prompt: "p", run_in_background: false }]);
	const launched = agent.renderResult!({ content: [], details: { status: "background" } } as never, { expanded: false } as never, {} as never, {} as never);
	assert.deepEqual(launched.render(80), []);
	const mentionCopy = cloudDelegationTool({ ...agent, execute: agent.execute });
	assert.equal(mentionCopy.description, agent.description, "an @mention's copy of the adapted Agent tool is not adapted twice");

	const workflow = cloudDelegationTool({
		name: "ct_workflow",
		description: "Concurrent agent() calls are capped at the configured session limit; excess calls queue. Nested workflows share this limit. effort?: string, opts.effort overrides these settings up to opts.isolation: agentType, model, effort, isolation",
		parameters: { type: "object", properties: {} },
		execute: async () => ({ content: [] }),
		renderResult: () => new Text("audit  2/5 agents · 12s\n┌ phase 1\n│ ├ ✓ scan", 0, 0),
	} as unknown as ToolDefinition);
	const render = (expanded: boolean) => workflow.renderResult!({ content: [], details: {} } as never, { expanded } as never, {} as never, {} as never).render(80).map((line) => line.trimEnd());
	assert.deepEqual(render(false), ["audit  2/5 agents · 12s"]);
	assert.equal(render(true).length, 3);
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

test("running agents and commands share one Tasks row, and the fleet panel lists commands only on demand", async () => {
	const { AgentWidget } = await import("@cloudthinker/subagents/src/ui/agent-widget.ts");
	const { FleetList } = await import("@cloudthinker/subagents/src/ui/fleet-list.ts");
	const { setSubagentHost } = await import("@cloudthinker/subagents/src/host.ts");
	const { subagentHost } = await import("../src/subagents.ts");
	const { tasksPane } = await import("../src/tasks-pane.ts");
	const widgets = new Map<string, unknown>();
	const statuses = new Map<string, string | undefined>();
	const keys: ((data: string) => { consume?: boolean } | undefined)[] = [];
	const stopped: string[] = [];
	const ui = {
		setWidget: (key: string, value: unknown) => { value === undefined ? widgets.delete(key) : widgets.set(key, value); },
		setStatus: (key: string, value: string | undefined) => { statuses.set(key, value); },
		onTerminalInput: (handler: (data: string) => { consume?: boolean } | undefined) => { keys.push(handler); return () => {}; },
		getEditorText: () => "",
		notify() {},
	};
	const theme = { fg: (_color: string, value: string) => value, bold: (value: string) => value };
	const render = (key: string) => {
		const factory = widgets.get(key) as ((tui: unknown, theme: unknown) => { render(width?: number): string[] }) | undefined;
		return factory?.({ terminal: { columns: 160 }, requestRender() {} }, theme).render(160) ?? [];
	};
	const agents = [{ id: "agent-1", type: "Explore", status: "running", description: "find render path", toolUses: 0, startedAt: Date.now(), session: {} }];
	const manager = { listAgents: () => agents, abort: () => true };
	setSubagentHost(subagentHost);
	tasksPane.managed = true;
	tasksPane.bind(ui as never);
	const widget = new AgentWidget(manager as never, new Map());
	widget.setUICtx(ui as never);
	widget.update();
	tasksPane.setCommands([{ id: "cmd-1", command: "pnpm test", startedAt: Date.now(), tail: () => "", output: () => "", running: () => true, stop: async () => { stopped.push("cmd-1"); } }]);
	assert.deepEqual(render("ct-tasks").map((line) => line.slice(2)), ["1 agent · 1 command  ↓ to manage"]);
	assert.equal(widgets.has("agents"), false);
	assert.equal(statuses.get("subagents"), undefined);

	const fleet = new FleetList(manager as never, new Map(), () => false);
	try {
		fleet.setUICtx(ui as never);
		fleet.update();
		assert.deepEqual(render("fleet"), []);
		assert.deepEqual(keys.at(-1)!("\u001b[B"), { consume: true });
		const panel = render("fleet").join("\n");
		assert.match(panel, /x stop/);
		assert.match(panel, /● \S+ {2}find render path/);
		assert.match(panel, /command {2}pnpm test/);
		keys.at(-1)!("\u001b[B");
		keys.at(-1)!("x");
		await new Promise((resolve) => setImmediate(resolve));
		assert.deepEqual(stopped, ["cmd-1"]);

		agents.length = 0;
		widget.update();
		tasksPane.setCommands([]);
		assert.equal(widgets.has("ct-tasks"), false);
	} finally {
		fleet.dispose();
		widget.dispose();
	}
});
