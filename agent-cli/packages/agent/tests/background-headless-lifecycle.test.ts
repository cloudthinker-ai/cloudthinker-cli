import assert from "node:assert/strict";
import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import {
	DefaultResourceLoader,
	SessionManager,
	SettingsManager,
	createAgentSession,
	type ModelRuntime,
} from "@earendil-works/pi-coding-agent";
import { runPrintMode } from "@earendil-works/pi-coding-agent";
import { registerBackgroundCommands } from "../src/background/index.ts";

const model = {
	api: "openai-completions",
	baseUrl: "http://127.0.0.1/unused",
	contextWindow: 4096,
	cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
	input: ["text"],
	maxTokens: 512,
	name: "Lifecycle test model",
	provider: "lifecycle-test",
	id: "fixture",
	reasoning: false,
};

type Scenario = {
	initialTasks: string[];
	completionTasks?: string[];
	responses: string[];
	mode?: "text" | "json";
	completionTiming?: "during_model" | "settled";
};

function shellQuote(path: string): string {
	return `'${path.replaceAll("'", "'\\''")}'`;
}

function commandFor(name: string, root: string, releases: Set<string>): string {
	const started = join(root, `${name}.started`);
	const finished = join(root, `${name}.finished`);
	const release = join(root, `${name}.release`);
	releases.add(release);
	return `printf started > ${shellQuote(started)}; while [ ! -f ${shellQuote(release)} ]; do sleep 0.01; done; printf finished > ${shellQuote(finished)}; printf ${shellQuote(name)}`;
}

async function waitForFiles(paths: string[]): Promise<void> {
	for (const path of paths) {
		for (let attempt = 0; attempt < 300 && !existsSync(path); attempt += 1) {
			await new Promise((resolve) => setTimeout(resolve, 10));
		}
		assert.ok(existsSync(path), `timed out waiting for ${path}`);
	}
}

async function waitForCondition(condition: () => boolean, description: string): Promise<void> {
	for (let attempt = 0; attempt < 300 && !condition(); attempt += 1) {
		await new Promise((resolve) => setTimeout(resolve, 10));
	}
	assert.ok(condition(), `timed out waiting for ${description}`);
}

type MessagePart = { type: "text"; text: string } | { type: "toolCall"; id: string; name: string; arguments: unknown };

function assistantMessage(content: MessagePart[], stopReason: "stop" | "toolUse" = "stop") {
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

function toolCallMessage(tasks: Array<{ name: string; command: string }>) {
	return assistantMessage(tasks.map(({ name, command }) => ({
		type: "toolCall",
		id: `call-${name}`,
		name: "ct_background",
		arguments: { action: "start", command },
	})), "toolUse");
}

function streamMessage(message: ReturnType<typeof assistantMessage>, waitFor: string[] = [], waitUntil?: () => boolean) {
	return {
		async *[Symbol.asyncIterator]() {
			let partial = { ...message, content: [] as MessagePart[], stopReason: "pending" };
			yield { type: "start", partial };
			for (let index = 0; index < message.content.length; index += 1) {
				const content = message.content[index]!;
				if (content.type !== "toolCall") continue;
				const incomplete: Extract<MessagePart, { type: "toolCall" }> = { type: "toolCall", id: content.id, name: content.name, arguments: {} };
				partial = { ...partial, content: [...partial.content, incomplete] };
				yield { type: "toolcall_start", contentIndex: index, partial };
				const complete = { ...incomplete, arguments: content.arguments };
				partial = { ...partial, content: partial.content.map((part, partIndex) => partIndex === index ? complete : part) };
				yield { type: "toolcall_end", contentIndex: index, toolCall: complete, partial };
			}
			await waitForFiles(waitFor);
			if (waitUntil) await waitForCondition(waitUntil, "background completion notification");
			yield { type: "done", reason: message.stopReason === "toolUse" ? "toolUse" : "stop", message };
		},
		result: async () => message,
	};
}

async function runScenario(scenario: Scenario) {
	const cwd = mkdtempSync(join(tmpdir(), "ct-background-headless-"));
	const agentDir = join(cwd, "agent");
	const oldAgentDir = process.env.PI_CODING_AGENT_DIR;
	process.env.PI_CODING_AGENT_DIR = agentDir;
	const settingsManager = SettingsManager.inMemory({});
	const releases = new Set<string>();
	const streamCalls: string[] = [];
	const pendingAtStream: number[] = [];
	const completionContexts: string[] = [];
	const completionSettlePhases: number[] = [];
	const markersSeen = new Set<string>();
	let completionTaskCount = 0;
	let settledCount = 0;
	let session: Awaited<ReturnType<typeof createAgentSession>>["session"] | undefined;
	let disposed = false;
	try {
		const resourceLoader = new DefaultResourceLoader({
			cwd,
			agentDir,
			settingsManager,
			noExtensions: true,
			noSkills: true,
			noPromptTemplates: true,
		noThemes: true,
		extensionFactories: [{ name: "background-commands", factory: (pi) => {
			pi.on("agent_settled", () => { settledCount += 1; });
			pi.on("agent_settled", async () => {
				if (scenario.completionTiming !== "settled" || settledCount !== 1) return;
				for (const name of scenario.initialTasks) {
					await waitForFiles([join(cwd, `${name}.started`)]);
					markersSeen.add(name);
					writeFileSync(join(cwd, `${name}.release`), "go");
				}
			});
			const backgroundPi = new Proxy(pi, {
				get(target, property, receiver) {
					if (property !== "sendMessage") return Reflect.get(target, property, receiver);
					return (message: Parameters<typeof pi.sendMessage>[0], options: Parameters<typeof pi.sendMessage>[1]) => {
						target.sendMessage(message, options);
						if (message.customType !== "ct_background_completion") return;
						const content = typeof message.content === "string"
							? message.content
							: message.content.filter((part) => part.type === "text").map((part) => part.text).join("");
						completionTaskCount += content.match(/^- /gm)?.length ?? 0;
					};
				},
			});
			registerBackgroundCommands(backgroundPi);
			pi.on("message_end", async (event) => {
				if (event.message.role !== "assistant") return;
				const text = event.message.content.filter((part) => part.type === "text").map((part) => part.text).join("");
				const names = scenario.completionTiming === "settled" ? [] : text === scenario.responses[0] ? scenario.initialTasks : text === scenario.responses[1] ? scenario.completionTasks ?? [] : [];
				for (const name of names) {
					for (let attempt = 0; attempt < 300 && !existsSync(join(cwd, `${name}.started`)); attempt += 1) {
						await new Promise((resolve) => setTimeout(resolve, 10));
					}
					if (existsSync(join(cwd, `${name}.started`))) markersSeen.add(name);
					writeFileSync(join(cwd, `${name}.release`), "go");
				}
			});
		} }],
		});
		await resourceLoader.reload();
		const modelRuntime = {
			hasConfiguredAuth: () => true,
			checkAuth: async () => ({ type: "api_key", source: "test" }),
			getAuth: async () => ({ auth: { apiKey: "test" }, env: {} }),
			isUsingOAuth: () => false,
			getModel: (provider: string, id: string) => provider === model.provider && id === model.id ? model : undefined,
			streamSimple: (_model: unknown, context: { messages: unknown[] }) => {
				const index = streamCalls.length;
				const serialized = JSON.stringify(context.messages);
				streamCalls.push(serialized);
				if (index > 1) {
					completionContexts.push(serialized);
					completionSettlePhases.push(settledCount);
				}
				let message: ReturnType<typeof assistantMessage>;
				if (index === 0 && scenario.initialTasks.length > 0) {
					message = toolCallMessage(scenario.initialTasks.map((name) => ({ name, command: commandFor(name, cwd, releases) })));
				} else if (index === 1 && scenario.initialTasks.length > 0) {
					pendingAtStream.push(scenario.initialTasks.length);
					message = assistantMessage([{ type: "text", text: scenario.responses[0]! }]);
				} else if (index === 2 && scenario.completionTasks?.length) {
					message = toolCallMessage(scenario.completionTasks.map((name) => ({ name, command: commandFor(name, cwd, releases) })));
				} else if (index === 3 && scenario.completionTasks?.length) {
					pendingAtStream.push(scenario.completionTasks.length);
					message = assistantMessage([{ type: "text", text: scenario.responses[1]! }]);
				} else {
					const responseIndex = scenario.initialTasks.length === 0
						? 0
						: scenario.completionTasks?.length
							? index - 2
							: Math.min(index - 1, scenario.responses.length - 1);
					message = assistantMessage([{ type: "text", text: scenario.responses[responseIndex] ?? "Unexpected extra turn" }]);
				}
				const releaseNames = index === 1 && scenario.completionTiming === "during_model"
					? scenario.initialTasks
					: index === 3 && scenario.completionTiming !== "settled"
						? scenario.completionTasks ?? []
						: [];
				for (const name of releaseNames) writeFileSync(join(cwd, `${name}.release`), "go");
				const waitFor = releaseNames.map((name) => join(cwd, `${name}.finished`));
				const completionTarget = index === 1
					? scenario.initialTasks.length
					: scenario.initialTasks.length + (scenario.completionTasks?.length ?? 0);
				const waitUntil = scenario.completionTiming === "during_model" && releaseNames.length > 0
					? () => completionTaskCount >= completionTarget
					: undefined;
				return streamMessage(message, waitFor, waitUntil);
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
		const runtimeHost = {
			get session() { return session!; },
			setRebindSession() {},
			async dispose() {
				await session!.extensionRunner.emit({ type: "session_shutdown", reason: "quit" });
				session!.dispose();
				disposed = true;
			},
		};
		const exitCode = await runPrintMode(runtimeHost as never, {
			mode: scenario.mode ?? "text",
			initialMessage: scenario.initialTasks.length > 0 ? "run background work" : "no background work",
		});
		const lastAssistant = [...session.state.messages].reverse().find((message) => message.role === "assistant");
		const finalText = lastAssistant?.role === "assistant" ? lastAssistant.content.filter((part) => part.type === "text").map((part) => part.text).join("") : "";
		return { exitCode, finalText, streamCalls, pendingAtStream, completionContexts, completionSettlePhases, markersSeen, disposed };
	} finally {
		for (const name of [...scenario.initialTasks, ...(scenario.completionTasks ?? [])]) writeFileSync(join(cwd, `${name}.release`), "go");
		session?.dispose();
		if (oldAgentDir === undefined) delete process.env.PI_CODING_AGENT_DIR;
		else process.env.PI_CODING_AGENT_DIR = oldAgentDir;
		rmSync(cwd, { recursive: true, force: true });
	}
}

test("print mode drains a real background command after the parent response", { timeout: 10_000 }, async () => {
	const result = await runScenario({ initialTasks: ["first"], responses: ["Parent continued", "Command completed"], completionTiming: "during_model" });
	assert.equal(result.exitCode, 0);
	assert.equal(result.finalText, "Command completed");
	assert.equal(result.streamCalls.length, 3);
	assert.deepEqual(result.pendingAtStream, [1]);
	assert.deepEqual(result.markersSeen, new Set(["first"]));
	assert.equal(result.completionContexts.length, 1);
	assert.deepEqual(result.completionSettlePhases, [0]);
	assert.match(result.completionContexts[0]!, /Background command completion event/);
	assert.equal(result.disposed, true);
});

test("JSON mode drains multiple background commands before final output", { timeout: 10_000 }, async () => {
	const result = await runScenario({
		mode: "json",
		initialTasks: ["first", "second"],
		responses: ["Parent continued", "Both completed"],
		completionTiming: "during_model",
	});
	assert.equal(result.exitCode, 0);
	assert.equal(result.finalText, "Both completed");
	assert.ok(result.streamCalls.length >= 3 && result.streamCalls.length <= 4);
	assert.deepEqual(result.pendingAtStream, [2]);
	assert.ok(result.completionContexts.length >= 1 && result.completionContexts.length <= 2);
	assert.ok(result.completionSettlePhases.every((phase) => phase === 0));
	assert.match(result.completionContexts[0]!, /Background command completion event/);
});

test("headless follow-up drains work started by a completion turn before final output", { timeout: 10_000 }, async () => {
	const result = await runScenario({
		initialTasks: ["first"],
		completionTasks: ["second"],
		responses: ["Parent continued", "Second task started", "All work completed"],
		completionTiming: "during_model",
	});
	assert.equal(result.finalText, "All work completed");
	assert.equal(result.streamCalls.length, 5);
	assert.deepEqual(result.pendingAtStream, [1, 1]);
	assert.equal(result.completionContexts.length, 3);
	assert.deepEqual(result.completionSettlePhases, [0, 0, 0]);
	assert.ok(result.completionContexts.every((context) => context.includes("Background command completion event")));
});

test("headless mode exits without an extra turn when no command was started", { timeout: 10_000 }, async () => {
	const result = await runScenario({ initialTasks: [], responses: ["No work was started"] });
	assert.equal(result.finalText, "No work was started");
	assert.equal(result.streamCalls.length, 1);
	assert.equal(result.completionContexts.length, 0);
	assert.equal(result.disposed, true);
});

test("headless mode awaits completion triggered as the parent run settles without a duplicate turn", { timeout: 10_000 }, async () => {
	const result = await runScenario({
		initialTasks: ["first"],
		responses: ["Parent finished", "Command completed"],
		completionTiming: "settled",
	});
	assert.equal(result.exitCode, 0);
	assert.equal(result.finalText, "Command completed");
	assert.equal(result.streamCalls.length, 3);
	assert.deepEqual(result.completionSettlePhases, [1]);
	assert.equal(result.disposed, true);
});
