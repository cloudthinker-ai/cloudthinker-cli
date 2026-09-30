import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import {
	DefaultResourceLoader,
	SessionManager,
	SettingsManager,
	createAgentSession,
	type ExtensionUIContext,
	type ModelRuntime,
} from "@earendil-works/pi-coding-agent";
import { registerBackgroundCommands } from "../src/background/index.ts";
import { BackgroundCommandManager } from "../src/background/manager.ts";

const model = {
	api: "openai-completions",
	baseUrl: "http://127.0.0.1/unused",
	contextWindow: 4096,
	cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
	input: ["text"],
	maxTokens: 512,
	name: "Background interruption model",
	provider: "background-interruption-test",
	id: "fixture",
	reasoning: false,
};

function quote(value: string): string {
	return `'${value.replaceAll("'", "'\\''")}'`;
}

function assistant(content: Array<{ type: "text"; text: string } | { type: "toolCall"; id: string; name: string; arguments: unknown }>, stopReason: "stop" | "toolUse" = "stop") {
	return {
		role: "assistant",
		content,
		api: model.api,
		provider: model.provider,
		model: model.id,
		usage: { input: 1, output: 1, cacheRead: 0, cacheWrite: 0, totalTokens: 2, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
		stopReason,
		timestamp: Date.now(),
	};
}

function stream(message: ReturnType<typeof assistant>, started?: () => void, gate?: Promise<void>) {
	return {
		async *[Symbol.asyncIterator]() {
			let partial = { ...message, content: [] as typeof message.content, stopReason: "pending" };
			yield { type: "start", partial };
			for (let index = 0; index < message.content.length; index += 1) {
				const content = message.content[index]!;
				if (content.type !== "toolCall") continue;
				const incomplete = { ...content, arguments: {} };
				partial = { ...partial, content: [...partial.content, incomplete] };
				yield { type: "toolcall_start", contentIndex: index, partial };
				partial = { ...partial, content: partial.content.map((part, partIndex) => partIndex === index ? content : part) };
				yield { type: "toolcall_end", contentIndex: index, toolCall: content, partial };
			}
			started?.();
			await gate;
			yield { type: "done", reason: message.stopReason === "toolUse" ? "toolUse" : "stop", message };
		},
		result: async () => message,
	};
}

test("an unknown task is reported as a failed Pi tool call", { timeout: 10_000 }, async () => {
	const cwd = mkdtempSync(join(tmpdir(), "ct-background-tool-error-"));
	const agentDir = join(cwd, "agent");
	const oldAgentDir = process.env.PI_CODING_AGENT_DIR;
	process.env.PI_CODING_AGENT_DIR = agentDir;
	const settingsManager = SettingsManager.inMemory({});
	const ui = { notify() {}, setStatus() {}, setWidget() {} } as unknown as ExtensionUIContext;
	let session: Awaited<ReturnType<typeof createAgentSession>>["session"] | undefined;
	const toolEvents: Array<{ isError?: boolean; result?: { isError?: boolean } }> = [];
	let unsubscribe = () => {};
	try {
		const resourceLoader = new DefaultResourceLoader({
			cwd, agentDir, settingsManager,
			noExtensions: true, noSkills: true, noPromptTemplates: true, noThemes: true,
			extensionFactories: [{ name: "background-commands", factory: (pi) => registerBackgroundCommands(pi) }],
		});
		await resourceLoader.reload();
		const modelRuntime = {
			hasConfiguredAuth: () => true,
			checkAuth: async () => ({ type: "api_key", source: "test" }),
			getAuth: async () => ({ auth: { apiKey: "test" }, env: {} }),
			isUsingOAuth: () => false,
			streamSimple: (_model: unknown, _context: { messages: unknown[] }) => {
				if (toolEvents.length === 0) return stream(assistant([{
					type: "toolCall",
					id: "unknown-task-output",
					name: "ct_background",
					arguments: { action: "output", taskId: "missing-task" },
				}], "toolUse"));
				return stream(assistant([{ type: "text", text: "Observed tool error" }]));
			},
		} as unknown as ModelRuntime;
		const created = await createAgentSession({
			cwd, agentDir, model: model as never, modelRuntime, settingsManager, resourceLoader,
			sessionManager: SessionManager.create(cwd), tools: ["ct_background"],
		});
		session = created.session;
		await session.bindExtensions({ mode: "tui", uiContext: ui });
		unsubscribe = session.subscribe((event) => {
			if (event.type === "tool_execution_end" && event.toolName === "ct_background") toolEvents.push(event);
		});
		await session.prompt("Read a missing task");
		assert.equal(toolEvents.length, 1);
		assert.equal(toolEvents[0]?.isError, true);
	} finally {
		unsubscribe();
		if (session) await session.extensionRunner.emit({ type: "session_shutdown", reason: "quit" });
		session?.dispose();
		if (oldAgentDir === undefined) delete process.env.PI_CODING_AGENT_DIR;
		else process.env.PI_CODING_AGENT_DIR = oldAgentDir;
		rmSync(cwd, { recursive: true, force: true });
	}
});

test("Esc restores a queued background completion with the current draft for Continue", { timeout: 10_000 }, async () => {
	const cwd = mkdtempSync(join(tmpdir(), "ct-background-interruption-"));
	const agentDir = join(cwd, "agent");
	const oldAgentDir = process.env.PI_CODING_AGENT_DIR;
	process.env.PI_CODING_AGENT_DIR = agentDir;
	let releaseParent!: () => void;
	let markParentStarted!: () => void;
	const parentGate = new Promise<void>((resolve) => { releaseParent = resolve; });
	const parentStarted = new Promise<void>((resolve) => { markParentStarted = resolve; });
	const modelInputs: string[] = [];
	const modelSystemPrompts: string[] = [];
	const settingsManager = SettingsManager.inMemory({});
	const ui = {
		notify() {}, setStatus() {}, setWidget() {},
	} as unknown as ExtensionUIContext;
	let session: Awaited<ReturnType<typeof createAgentSession>>["session"] | undefined;
	try {
		const resourceLoader = new DefaultResourceLoader({
			cwd,
			agentDir,
			settingsManager,
			noExtensions: true,
			noSkills: true,
			noPromptTemplates: true,
			noThemes: true,
			extensionFactories: [{ name: "background-commands", factory: (pi) => registerBackgroundCommands(pi) }],
		});
		await resourceLoader.reload();
		const modelRuntime = {
			hasConfiguredAuth: () => true,
			checkAuth: async () => ({ type: "api_key", source: "test" }),
			getAuth: async () => ({ auth: { apiKey: "test" }, env: {} }),
			isUsingOAuth: () => false,
			streamSimple: (_model: unknown, context: { messages: unknown[] }) => {
				const index = modelInputs.length;
				modelInputs.push(JSON.stringify(context.messages));
				modelSystemPrompts.push(session?.systemPrompt ?? "");
				if (index === 0) {
					const taskOutput = join(cwd, "report.txt");
					return stream(assistant([{
						type: "toolCall",
						id: "start-background",
						name: "ct_background",
						arguments: { action: "start", command: `sleep 0.1; printf saved-report > ${quote(taskOutput)}` },
					}], "toolUse"));
				}
				if (index === 1) return stream(assistant([{ type: "text", text: "Outer work response" }]), markParentStarted, parentGate);
				return stream(assistant([{ type: "text", text: "Continued with saved report" }]));
			},
		} as unknown as ModelRuntime;
		const created = await createAgentSession({
			cwd,
			agentDir,
			model: model as never,
			modelRuntime,
			settingsManager,
			resourceLoader,
			sessionManager: SessionManager.create(cwd),
			tools: ["ct_background"],
		});
		session = created.session;
		await session.bindExtensions({ mode: "tui", uiContext: ui });

		const outerRun = session.prompt("Start the local report command");
		await parentStarted;
		for (let attempt = 0; attempt < 300 && session.pendingMessageCount === 0; attempt += 1) await new Promise((resolve) => setTimeout(resolve, 10));
		assert.equal(session.pendingMessageCount, 1, "terminal background event should be queued as a Pi follow-up");

		const { followUp } = session.clearQueue();
		assert.equal(followUp.length, 1);
		assert.match(followUp[0]!, /Background command completion event/);
		const editorDraft = "Keep these findings in the report draft.";
		const restoredDraft = [followUp[0]!, editorDraft].filter((text) => text.trim()).join("\n\n");
		releaseParent();
		await session.abort();
		await outerRun;
		assert.equal(session.pendingMessageCount, 0);
		assert.equal(readFileSync(join(cwd, "report.txt"), "utf8"), "saved-report");

		await session.prompt(restoredDraft);
		assert.equal(modelInputs.length, 3);
		assert.match(modelSystemPrompts[0]!, /Use ct_background when a command can run alongside other work; continue useful work until its completion event\./);
		assert.match(modelInputs[2]!, /Background command completion event/);
		assert.match(modelInputs[2]!, /Keep these findings in the report draft/);
		assert.equal((modelInputs[2]!.match(/Background command completion event/g) ?? []).length, 1);
		assert.match(session.getLastAssistantText() ?? "", /Continued with saved report/);
		assert.equal(session.pendingMessageCount, 0);
	} finally {
		releaseParent();
		if (session) await session.extensionRunner.emit({ type: "session_shutdown", reason: "quit" });
		session?.dispose();
		if (oldAgentDir === undefined) delete process.env.PI_CODING_AGENT_DIR;
		else process.env.PI_CODING_AGENT_DIR = oldAgentDir;
		rmSync(cwd, { recursive: true, force: true });
	}
});

test("recovered task context survives Esc and remains available on Continue", { timeout: 10_000 }, async () => {
	const cwd = mkdtempSync(join(tmpdir(), "ct-background-recovery-continue-"));
	const agentDir = join(cwd, "agent");
	const oldAgentDir = process.env.PI_CODING_AGENT_DIR;
	process.env.PI_CODING_AGENT_DIR = agentDir;
	let releaseFirst!: () => void;
	let markFirstStarted!: () => void;
	const firstGate = new Promise<void>((resolve) => { releaseFirst = resolve; });
	const firstStarted = new Promise<void>((resolve) => { markFirstStarted = resolve; });
	const modelInputs: string[] = [];
	const settingsManager = SettingsManager.inMemory({});
	const ui = { notify() {}, setStatus() {}, setWidget() {} } as unknown as ExtensionUIContext;
	let session: Awaited<ReturnType<typeof createAgentSession>>["session"] | undefined;
	try {
		const sessionManager = SessionManager.create(cwd);
		const directory = BackgroundCommandManager.sessionStorageDirectory(agentDir, sessionManager.getSessionId());
		mkdirSync(directory, { recursive: true, mode: 0o700 });
		writeFileSync(join(directory, "tasks.json"), JSON.stringify({
			version: 1,
			tasks: [{
				id: "recovered1234ab",
				command: "printf saved-report",
				cwd,
				state: "succeeded",
				createdAt: 1,
				finishedAt: 2,
				exitCode: 0,
				outputBaseByte: 0,
				totalOutputBytes: 12,
				output: Buffer.from("saved-report").toString("base64"),
				completionDelivered: false,
			}],
		}));
		const resourceLoader = new DefaultResourceLoader({
			cwd,
			agentDir,
			settingsManager,
			noExtensions: true,
			noSkills: true,
			noPromptTemplates: true,
			noThemes: true,
			extensionFactories: [{ name: "background-commands", factory: (pi) => registerBackgroundCommands(pi) }],
		});
		await resourceLoader.reload();
		const modelRuntime = {
			hasConfiguredAuth: () => true,
			checkAuth: async () => ({ type: "api_key", source: "test" }),
			getAuth: async () => ({ auth: { apiKey: "test" }, env: {} }),
			isUsingOAuth: () => false,
			streamSimple: (_model: unknown, context: { messages: unknown[] }) => {
				const index = modelInputs.length;
				modelInputs.push(JSON.stringify(context.messages));
				if (index === 0) return stream(assistant([{ type: "text", text: "First attempt stopped" }]), markFirstStarted, firstGate);
				return stream(assistant([{ type: "text", text: "Recovered report reviewed" }]));
			},
		} as unknown as ModelRuntime;
		const created = await createAgentSession({
			cwd,
			agentDir,
			model: model as never,
			modelRuntime,
			settingsManager,
			resourceLoader,
			sessionManager,
			tools: ["ct_background"],
		});
		session = created.session;
		await session.bindExtensions({ mode: "tui", uiContext: ui });

		const firstRun = session.prompt("Resume interrupted work");
		await firstStarted;
		assert.match(modelInputs[0]!, /Recovered background command state/);
		assert.match(modelInputs[0]!, /recovered1234ab: succeeded/);
		assert.deepEqual(session.clearQueue(), { steering: [], followUp: [] });
		releaseFirst();
		await session.abort();
		await firstRun;

		await session.prompt("Continue");
		assert.equal(modelInputs.length, 2);
		assert.match(modelInputs[1]!, /Recovered background command state/);
		assert.match(modelInputs[1]!, /recovered1234ab: succeeded/);
		assert.equal((modelInputs[1]!.match(/Recovered background command state/g) ?? []).length, 1);
		assert.equal(session.getLastAssistantText(), "Recovered report reviewed");
	} finally {
		releaseFirst();
		if (session) await session.extensionRunner.emit({ type: "session_shutdown", reason: "quit" });
		session?.dispose();
		if (oldAgentDir === undefined) delete process.env.PI_CODING_AGENT_DIR;
		else process.env.PI_CODING_AGENT_DIR = oldAgentDir;
		rmSync(cwd, { recursive: true, force: true });
	}
});
