import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import type { ExtensionAPI, ExtensionContext, ToolDefinition } from "@earendil-works/pi-coding-agent";
import { registerBackgroundCommands } from "../src/background/index.ts";
import { BackgroundCommandManager } from "../src/background/manager.ts";

interface TestUI {
	status: Map<string, string | undefined>;
	widgets: Map<string, unknown>;
}

const plainTheme = { fg: (_color: string, value: string) => value, bold: (value: string) => value } as never;

function renderTasks(ui: TestUI): string {
	const factory = ui.widgets.get("ct-tasks") as ((tui: unknown, theme: unknown) => { render(): string[] }) | undefined;
	if (!factory) return "";
	return factory({ terminal: { columns: 200 }, requestRender() {} }, plainTheme).render().join("\n");
}

function createContext(id: string, mode: ExtensionContext["mode"], cwd: string, ui: TestUI): ExtensionContext {
	return {
		mode,
		hasUI: mode === "tui" || mode === "rpc",
		cwd,
		sessionManager: { getSessionId: () => id },
		ui: {
			setStatus: (key: string, value: string | undefined) => { ui.status.set(key, value); },
			setWidget: (key: string, value: unknown) => { ui.widgets.set(key, value); },
		} as unknown as ExtensionContext["ui"],
	} as ExtensionContext;
}

function createApi() {
	const handlers = new Map<string, (event: unknown, ctx: ExtensionContext) => unknown>();
	const messages: unknown[] = [];
	let tool: ToolDefinition | undefined;
	const api = {
		registerTool: (registered: ToolDefinition) => { tool = registered; },
		on: (event: string, handler: (event: unknown, ctx: ExtensionContext) => unknown) => { handlers.set(event, handler); },
		sendMessage: (message: unknown) => { messages.push(message); },
		sendUserMessage: (message: unknown) => { messages.push(message); },
	} as unknown as ExtensionAPI;
	registerBackgroundCommands(api);
	assert.ok(tool);
	return { handlers, messages, tool: tool as ToolDefinition };
}

function assertSafeDisplay(value: string): void {
	assert.doesNotMatch(value, /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f\u061c\u200e\u200f\u202a-\u202e\u2066-\u206f]/u);
}

async function execute(tool: ToolDefinition, ctx: ExtensionContext, params: Record<string, unknown>) {
	return tool.execute("fixture", params, undefined, undefined, ctx);
}

test("registration exposes start/status/output/cancel and TUI progress", async () => {
	const root = await mkdtemp(join(tmpdir(), "ct-background-registration-"));
	const oldAgentDir = process.env.PI_CODING_AGENT_DIR;
	process.env.PI_CODING_AGENT_DIR = root;
	const ui = { status: new Map(), widgets: new Map() } as TestUI;
	const ctx = createContext("session-a", "tui", root, ui);
	const api = createApi();
	try {
		assert.equal(api.tool.label, "Background command");
		const unsafeCommand = "echo ready \u001b[2J \u001b]52;c;secret\u0007 \u202eRTL\u2066";
		assert.equal(api.tool.renderShell, "self");
		const call = api.tool.renderCall?.({ action: "start", command: unsafeCommand }, plainTheme, {} as never);
		assert.match(call?.render(80).join("\n") ?? "", /● Background echo ready/);
		assertSafeDisplay(call?.render(80).join("\n") ?? "");
		const result = api.tool.renderResult?.({ content: [{ type: "text", text: "running\n" + "x".repeat(500) }], details: undefined }, { expanded: false, isPartial: false } as never, plainTheme, {} as never);
		const renderedResult = result?.render(1000) ?? [];
		assert.match(renderedResult.join("\n"), /x{100}/);
		assert.doesNotMatch(renderedResult.join("\n"), /x{321}/);
		const expanded = api.tool.renderResult?.({ content: [{ type: "text", text: "running\n" + "x".repeat(500) }], details: undefined }, { expanded: true, isPartial: false } as never, plainTheme, {} as never);
		assert.match(expanded?.render(1000).join("\n") ?? "", /x{500}/);
		const unsafeOutput = "plain\nline\t\u001b[2JCSI\u001b]52;c;secret\u0007OSC\u001b]8;;url\u001b\\link\u0000\u0007\u007f\u0085\u009bC1\u202eRTL\u2066ISO";
		const unsafeResult = { content: [{ type: "text" as const, text: unsafeOutput }], details: { raw: unsafeOutput } };
		const collapsedUnsafe = api.tool.renderResult?.(unsafeResult, { expanded: false, isPartial: false } as never, plainTheme, {} as never);
		const expandedUnsafe = api.tool.renderResult?.(unsafeResult, { expanded: true, isPartial: false } as never, plainTheme, {} as never);
		assertSafeDisplay(collapsedUnsafe?.render(200).join("\n") ?? "");
		const expandedText = expandedUnsafe?.render(200).join("\n") ?? "";
		assertSafeDisplay(expandedText);
		assert.match(expandedText, /plain/);
		assert.match(expandedText, /line/);
		assert.equal(unsafeResult.details.raw, unsafeOutput);
		const lines = Array.from({ length: 8 }, (_, index) => `line ${index + 1}`).join("\n");
		const outputContext = { args: { action: "output" } } as never;
		const preview = api.tool.renderResult?.({ content: [{ type: "text", text: lines }], details: { text: lines } }, { expanded: false, isPartial: false } as never, plainTheme, outputContext)?.render(200).map((line) => line.trim()) ?? [];
		assert.deepEqual(preview, ["… (3 earlier lines)", "line 4", "line 5", "line 6", "line 7", "line 8"]);
		const startResult = api.tool.renderResult?.({ content: [{ type: "text", text: "Started background command abc." }], details: {} }, { expanded: false, isPartial: false } as never, plainTheme, { args: { action: "start" } } as never);
		assert.deepEqual(startResult?.render(200), []);
		await api.handlers.get("session_start")?.({ type: "session_start", reason: "startup" }, ctx);
		const widgetUnsafeCommand = "printf 'ready'; sleep 30 # \u001b[2J \u001b]52;c;secret\u0007 \u202eRTL\u2066";
		const started = await execute(api.tool, ctx, { action: "start", command: widgetUnsafeCommand }) as { details: { task: { id: string } } };
		const id = started.details.task.id;
		assert.equal(ui.status.get("ct-background"), "1 running command");
		const widget = renderTasks(ui);
		assert.match(widget, /^● Tasks\n▾ Commands 1\n└─ \S printf 'ready'; sleep 30/);
		assert.match(widget, /⎿ {2}(ready|waiting for output…)/);
		assertSafeDisplay(widget);
		let output = await execute(api.tool, ctx, { action: "output", taskId: id }) as { details: { text: string } };
		for (let attempt = 0; attempt < 50 && output.details.text.length === 0; attempt++) {
			await new Promise((resolve) => setTimeout(resolve, 10));
			output = await execute(api.tool, ctx, { action: "output", taskId: id }) as { details: { text: string } };
		}
		assert.equal(output.details.text, "ready");
		const stopped = await execute(api.tool, ctx, { action: "cancel", taskId: id }) as { details: { task: { state: string } } };
		assert.equal(stopped.details.task.state, "cancelled");
		assert.equal(api.messages.length, 0);
	} finally {
		await api.handlers.get("session_shutdown")?.({ type: "session_shutdown", reason: "quit" }, ctx);
		if (oldAgentDir === undefined) delete process.env.PI_CODING_AGENT_DIR;
		else process.env.PI_CODING_AGENT_DIR = oldAgentDir;
		await rm(root, { recursive: true, force: true });
	}
});

test("completed background commands leave the TUI widget while status and output remain available", async () => {
	const root = await mkdtemp(join(tmpdir(), "ct-background-widget-completion-"));
	const oldAgentDir = process.env.PI_CODING_AGENT_DIR;
	process.env.PI_CODING_AGENT_DIR = root;
	const ui = { status: new Map(), widgets: new Map() } as TestUI;
	const ctx = createContext("widget-session", "tui", root, ui);
	const api = createApi();
	try {
		await api.handlers.get("session_start")?.({ type: "session_start", reason: "startup" }, ctx);
		const active = await execute(api.tool, ctx, { action: "start", command: "sleep 30" }) as { details: { task: { id: string } } };
		const finished = await execute(api.tool, ctx, { action: "start", command: "printf ready" }) as { details: { task: { id: string } } };
		let status = await execute(api.tool, ctx, { action: "status", taskId: finished.details.task.id }) as { details: { tasks: { state: string } } };
		for (let attempt = 0; attempt < 200 && status.details.tasks.state === "running"; attempt++) {
			await new Promise((resolve) => setTimeout(resolve, 10));
			status = await execute(api.tool, ctx, { action: "status", taskId: finished.details.task.id }) as typeof status;
		}
		assert.equal(status.details.tasks.state, "succeeded");
		const widget = renderTasks(ui);
		assert.match(widget, /sleep 30/);
		assert.ok(!widget.includes("printf ready"));
		assert.equal(ui.status.get("ct-background"), "1 running command");
		const output = await execute(api.tool, ctx, { action: "output", taskId: finished.details.task.id }) as { details: { text: string } };
		assert.equal(output.details.text, "ready");
		for (let attempt = 0; attempt < 200 && api.messages.length === 0; attempt++) {
			await new Promise((resolve) => setTimeout(resolve, 10));
		}
		assert.equal(api.messages.length, 1);
		await execute(api.tool, ctx, { action: "cancel", taskId: active.details.task.id });
		assert.equal(ui.widgets.get("ct-tasks"), undefined);
		assert.equal(ui.status.get("ct-background"), undefined);
		await api.handlers.get("session_shutdown")?.({ type: "session_shutdown", reason: "quit" }, ctx);
		await api.handlers.get("session_start")?.({ type: "session_start", reason: "resume" }, ctx);
		assert.equal(ui.widgets.get("ct-tasks"), undefined);
		const recovered = await execute(api.tool, ctx, { action: "output", taskId: finished.details.task.id }) as { details: { text: string } };
		assert.equal(recovered.details.text, "ready");
	} finally {
		await api.handlers.get("session_shutdown")?.({ type: "session_shutdown", reason: "quit" }, ctx);
		if (oldAgentDir === undefined) delete process.env.PI_CODING_AGENT_DIR;
		else process.env.PI_CODING_AGENT_DIR = oldAgentDir;
		await rm(root, { recursive: true, force: true });
	}
});

test("forked sessions cannot read or cancel parent background commands", async () => {
	const root = await mkdtemp(join(tmpdir(), "ct-background-fork-"));
	const oldAgentDir = process.env.PI_CODING_AGENT_DIR;
	process.env.PI_CODING_AGENT_DIR = root;
	const ui = { status: new Map(), widgets: new Map() } as TestUI;
	const parent = createContext("parent-session", "tui", root, ui);
	const fork = createContext("fork-session", "tui", root, ui);
	const api = createApi();
	try {
		await api.handlers.get("session_start")?.({ type: "session_start", reason: "startup" }, parent);
		const started = await execute(api.tool, parent, { action: "start", command: "sleep 30" }) as { details: { task: { id: string } } };
		const taskId = started.details.task.id;
		await api.handlers.get("session_start")?.({ type: "session_start", reason: "fork" }, fork);
		const status = await execute(api.tool, fork, { action: "status" }) as { details: { tasks: unknown[] } };
		assert.deepEqual(status.details.tasks, []);
		await assert.rejects(execute(api.tool, fork, { action: "cancel", taskId }), /Unknown background task/);
		assert.equal(api.messages.length, 0);
	} finally {
		await api.handlers.get("session_shutdown")?.({ type: "session_shutdown", reason: "quit" }, fork);
		await api.handlers.get("session_shutdown")?.({ type: "session_shutdown", reason: "quit" }, parent);
		if (oldAgentDir === undefined) delete process.env.PI_CODING_AGENT_DIR;
		else process.env.PI_CODING_AGENT_DIR = oldAgentDir;
		await rm(root, { recursive: true, force: true });
	}
});

test("headless settled waits for tasks and emits one terminal continuation", async () => {
	const root = await mkdtemp(join(tmpdir(), "ct-background-headless-"));
	const oldAgentDir = process.env.PI_CODING_AGENT_DIR;
	process.env.PI_CODING_AGENT_DIR = root;
	const ui = { status: new Map(), widgets: new Map() } as TestUI;
	const ctx = createContext("headless-session", "print", root, ui);
	const api = createApi();
	try {
		await api.handlers.get("session_start")?.({ type: "session_start", reason: "startup" }, ctx);
		await execute(api.tool, ctx, { action: "start", command: "sleep 0.05; printf 'finished'" });
		await api.handlers.get("agent_settled")?.({ type: "agent_settled" }, ctx);
		assert.equal(api.messages.length, 1);
		assert.match(JSON.stringify(api.messages[0]), /finished/);
		await api.handlers.get("agent_settled")?.({ type: "agent_settled" }, ctx);
		assert.equal(api.messages.length, 1);
	} finally {
		await api.handlers.get("session_shutdown")?.({ type: "session_shutdown", reason: "quit" }, ctx);
		if (oldAgentDir === undefined) delete process.env.PI_CODING_AGENT_DIR;
		else process.env.PI_CODING_AGENT_DIR = oldAgentDir;
		await rm(root, { recursive: true, force: true });
	}
});

test("headless next turn receives one recovery notice for interrupted work", async () => {
	const root = await mkdtemp(join(tmpdir(), "ct-background-recovery-registration-"));
	const oldAgentDir = process.env.PI_CODING_AGENT_DIR;
	process.env.PI_CODING_AGENT_DIR = root;
	const directory = BackgroundCommandManager.sessionStorageDirectory(root, "recovery-session");
	await mkdir(directory, { recursive: true, mode: 0o700 });
	await writeFile(join(directory, "tasks.json"), JSON.stringify({
		version: 1,
		tasks: [{ id: "task-1", command: "cloudthinker cyber discover RUN", cwd: root, state: "running", createdAt: 1, outputBaseByte: 0, totalOutputBytes: 0, output: "", completionDelivered: false }],
	}));
	const ctx = createContext("recovery-session", "print", root, { status: new Map(), widgets: new Map() });
	const api = createApi();
	try {
		await api.handlers.get("session_start")?.({ type: "session_start", reason: "resume" }, ctx);
		const recovery = await api.handlers.get("before_agent_start")?.({ type: "before_agent_start", prompt: "continue", systemPrompt: "", systemPromptOptions: {} }, ctx) as { message?: { content: Array<{ type: string; text?: string }> } } | undefined;
		assert.match(recovery?.message?.content.map((item) => item.text ?? "").join("\n") ?? "", /interrupted.*cloudthinker cyber discover RUN/);
		assert.equal(await api.handlers.get("before_agent_start")?.({ type: "before_agent_start", prompt: "next", systemPrompt: "", systemPromptOptions: {} }, ctx), undefined);
	} finally {
		await api.handlers.get("session_shutdown")?.({ type: "session_shutdown", reason: "quit" }, ctx);
		if (oldAgentDir === undefined) delete process.env.PI_CODING_AGENT_DIR;
		else process.env.PI_CODING_AGENT_DIR = oldAgentDir;
		await rm(root, { recursive: true, force: true });
	}
});
