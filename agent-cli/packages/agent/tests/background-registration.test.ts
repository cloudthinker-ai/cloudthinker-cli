import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import type { ExtensionAPI, ExtensionContext, ExtensionToolContext, ToolDefinition } from "@earendil-works/pi-coding-agent";
import { completionRows, registerBackgroundCommands } from "../src/background/index.ts";
import { queuedLines } from "../src/queue-ui.ts";
import { tasksPane } from "../src/tasks-pane.ts";
import { BackgroundCommandManager } from "../src/background/manager.ts";
import { setToolOutputMode } from "../src/verbosity.ts";

interface TestUI {
	status: Map<string, string | undefined>;
	widgets: Map<string, unknown>;
	keys?: ((data: string) => { consume?: boolean } | undefined)[];
}

const plainTheme = { fg: (_color: string, value: string) => value, bold: (value: string) => value } as never;

function renderTasks(ui: TestUI): string {
	const factory = ui.widgets.get("ct-tasks") as ((tui: unknown, theme: unknown) => { render(): string[] }) | undefined;
	if (!factory) return "";
	return factory({ terminal: { columns: 200 }, requestRender() {} }, plainTheme).render().join("\n");
}

function createContext(id: string, mode: ExtensionContext["mode"], cwd: string, ui: TestUI): ExtensionToolContext {
	return {
		mode,
		hasUI: mode === "tui" || mode === "rpc",
		cwd,
		sessionManager: { getSessionId: () => id, getSessionFile: () => undefined },
		ui: {
			setStatus: (key: string, value: string | undefined) => { ui.status.set(key, value); },
			setWidget: (key: string, value: unknown) => { ui.widgets.set(key, value); },
			onTerminalInput: (handler: (data: string) => { consume?: boolean } | undefined) => {
				(ui.keys ??= []).push(handler);
				return () => { ui.keys = ui.keys?.filter((item) => item !== handler); };
			},
		} as unknown as ExtensionContext["ui"],
	} as ExtensionToolContext;
}

function createApi() {
	const handlers = new Map<string, (event: unknown, ctx: ExtensionContext) => unknown>();
	const messages: unknown[] = [];
	const tools = new Map<string, ToolDefinition>();
	const renderers = new Map<string, (message: unknown, options: unknown, theme: unknown) => { render(width: number): string[] } | undefined>();
	const api = {
		registerMessageRenderer: (type: string, renderer: never) => { renderers.set(type, renderer); },
		registerTool: (registered: ToolDefinition) => { tools.set(registered.name, registered); },
		on: (event: string, handler: (event: unknown, ctx: ExtensionContext) => unknown) => { handlers.set(event, handler); },
		sendMessage: (message: unknown) => { messages.push(message); },
		sendUserMessage: (message: unknown) => { messages.push(message); },
	} as unknown as ExtensionAPI;
	registerBackgroundCommands(api);
	assert.ok(tools.has("ct_background") && tools.has("bash"));
	return { handlers, messages, renderers, tool: tools.get("ct_background")!, bash: tools.get("bash")! };
}

function assertSafeDisplay(value: string): void {
	assert.doesNotMatch(value, /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f\u061c\u200e\u200f\u202a-\u202e\u2066-\u206f]/u);
}

async function execute(tool: ToolDefinition, ctx: ExtensionToolContext, params: Record<string, unknown>) {
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
		const outputContext = { args: { action: "output", taskId: "abc" }, state: {} } as never;
		const outputRow = api.tool.renderCall?.({ action: "output", taskId: "abc" }, plainTheme, outputContext);
		const compactOutput = api.tool.renderResult?.({ content: [{ type: "text", text: lines }], details: { text: lines } }, { expanded: false, isPartial: false } as never, plainTheme, outputContext);
		assert.deepEqual(compactOutput?.render(200), []);
		assert.deepEqual(outputRow?.render(200), ["● Background output abc · 8 lines"]);
		setToolOutputMode("preview");
		const preview = api.tool.renderResult?.({ content: [{ type: "text", text: lines }], details: { text: lines } }, { expanded: false, isPartial: false } as never, plainTheme, outputContext)?.render(200).map((line) => line.trim()) ?? [];
		setToolOutputMode("compact");
		assert.deepEqual(preview, ["… (3 earlier lines)", "line 4", "line 5", "line 6", "line 7", "line 8"]);
		assert.deepEqual(outputRow?.render(200), ["● Background output abc"]);
		const startResult = api.tool.renderResult?.({ content: [{ type: "text", text: "Started background command abc." }], details: {} }, { expanded: false, isPartial: false } as never, plainTheme, { args: { action: "start" } } as never);
		assert.deepEqual(startResult?.render(200), []);
		await api.handlers.get("session_start")?.({ type: "session_start", reason: "startup" }, ctx);
		const widgetUnsafeCommand = "printf 'ready'; sleep 30 # \u001b[2J \u001b]52;c;secret\u0007 \u202eRTL\u2066";
		const started = await execute(api.tool, ctx, { action: "start", command: widgetUnsafeCommand }) as { details: { task: { id: string } } };
		const id = started.details.task.id;
		const widget = renderTasks(ui);
		assert.match(widget, /^\S \$ printf 'ready'; sleep 30 .* · \d+s/);
		assert.equal(widget.split("\n").length, 1);
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
		const output = await execute(api.tool, ctx, { action: "output", taskId: finished.details.task.id }) as { details: { text: string } };
		assert.equal(output.details.text, "ready");
		for (let attempt = 0; attempt < 200 && api.messages.length === 0; attempt++) {
			await new Promise((resolve) => setTimeout(resolve, 10));
		}
		assert.equal(api.messages.length, 1);
		assert.deepEqual(completionRows(api.messages[0] as string, plainTheme), [` printf ready · ${(api.messages[0] as string).match(/finished in \d+s/)![0]}`]);
		await tasksPane.commands.find((task) => task.id === active.details.task.id)!.stop();
		assert.equal(api.messages.length, 2);
		assert.match(api.messages[1] as string, new RegExp(`^- ${active.details.task.id} stopped by the user after \\d+s · sleep 30$`, "m"));
		assert.deepEqual(completionRows(api.messages[1] as string, plainTheme)?.map((row) => row.replace(/\d+s$/, "Ns")), [" sleep 30 · stopped by the user after Ns"]);
		const stoppedStatus = await execute(api.tool, ctx, { action: "status", taskId: active.details.task.id }) as { content: { text: string }[] };
		assert.match(stoppedStatus.content[0]!.text, / · stopped by the user · /);
		assert.deepEqual(queuedLines([], [api.messages[1] as string], "", plainTheme).map((line) => line.replace(/\d+s · /, "Ns · ")), ["Queued 1", "  › sleep 30 · stopped by the user after Ns · after this turn"]);
		assert.equal(ui.widgets.get("ct-tasks"), undefined);
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

test("a TUI bash call still running after the wait moves to the background without restarting, and Ctrl+B moves one at once", async () => {
	const root = await mkdtemp(join(tmpdir(), "ct-background-auto-"));
	const oldAgentDir = process.env.PI_CODING_AGENT_DIR;
	const oldSeconds = process.env.CLOUDTHINKER_AUTO_BACKGROUND_SECONDS;
	process.env.PI_CODING_AGENT_DIR = root;
	process.env.CLOUDTHINKER_AUTO_BACKGROUND_SECONDS = "0.3";
	const ui = { status: new Map(), widgets: new Map() } as TestUI;
	const ctx = createContext("auto-session", "tui", root, ui);
	const api = createApi();
	const bash = (command: string, context = ctx) => api.bash.execute("call", { command }, undefined, undefined, context) as Promise<{ content: { text: string }[]; details?: { backgroundTaskId?: string } }>;
	try {
		await api.handlers.get("session_start")?.({ type: "session_start", reason: "startup" }, ctx);
		const quick = await bash("printf quick");
		assert.equal(quick.content[0]!.text, "quick");
		assert.equal(quick.details?.backgroundTaskId, undefined);

		const marker = join(root, "runs");
		const moved = await bash(`echo run >> ${marker}; printf before; sleep 1.2; printf after`);
		const taskId = moved.details?.backgroundTaskId;
		assert.ok(taskId);
		assert.match(moved.content[0]!.text, new RegExp(`^before\\n\\nStill running, so it moved to background command ${taskId}`));
		assert.match(renderTasks(ui), /\$ echo run/);
		for (let attempt = 0; attempt < 300 && api.messages.length === 0; attempt++) await new Promise((resolve) => setTimeout(resolve, 10));
		assert.match(String(api.messages[0]), new RegExp(`${taskId} finished in 1s`));
		const output = await execute(api.tool, ctx, { action: "output", taskId }) as { details: { text: string } };
		assert.equal(output.details.text, "beforeafter");
		assert.equal((await readFile(marker, "utf8")).trim().split("\n").length, 1);

		process.env.CLOUDTHINKER_AUTO_BACKGROUND_SECONDS = "0";
		const held = bash("printf held; sleep 30");
		await new Promise((resolve) => setTimeout(resolve, 200));
		assert.deepEqual(ui.keys!.map((handler) => handler("\u0002")), [{ consume: true }]);
		const pressed = await held;
		assert.ok(pressed.details?.backgroundTaskId);
		assert.deepEqual(ui.keys!.map((handler) => handler("\u0002")), [undefined]);
		await execute(api.tool, ctx, { action: "cancel", taskId: pressed.details!.backgroundTaskId });

		const printCtx = createContext("auto-print-session", "print", root, { status: new Map(), widgets: new Map() });
		process.env.CLOUDTHINKER_AUTO_BACKGROUND_SECONDS = "0.1";
		const waited = await bash("sleep 0.3; printf waited", printCtx);
		assert.equal(waited.content[0]!.text, "waited");
		assert.equal(waited.details?.backgroundTaskId, undefined);
	} finally {
		await api.handlers.get("session_shutdown")?.({ type: "session_shutdown", reason: "quit" }, ctx);
		if (oldAgentDir === undefined) delete process.env.PI_CODING_AGENT_DIR;
		else process.env.PI_CODING_AGENT_DIR = oldAgentDir;
		if (oldSeconds === undefined) delete process.env.CLOUDTHINKER_AUTO_BACKGROUND_SECONDS;
		else process.env.CLOUDTHINKER_AUTO_BACKGROUND_SECONDS = oldSeconds;
		await rm(root, { recursive: true, force: true });
	}
});

test("a recorded background completion message resumes as one row per command", () => {
	const { renderers } = createApi();
	const render = renderers.get("ct_background_completion")!;
	const content = [{ type: "text", text: "Background command completion event\n- abc123 finished in 2s · make build\n- def456 stopped by the user after 5s · sleep 30\nRead each output with ct_background(action=\"output\", taskId=\"...\") before reporting." }];
	const lines = render({ content }, {}, plainTheme)!.render(80);
	assert.deepEqual(lines, ["", " make build · finished in 2s", " sleep 30 · stopped by the user after 5s"]);
	assert.equal(render({ content: [{ type: "text", text: "unrelated" }] }, {}, plainTheme), undefined);
});
