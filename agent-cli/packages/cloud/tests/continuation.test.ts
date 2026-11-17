import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import {
	DefaultResourceLoader,
	SessionManager,
	SettingsManager,
	createAgentSession,
	runPrintMode,
	type ModelRuntime,
} from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";

import { CONTINUE_PROMPT, LIMIT_NOTICE, MAX_CONTINUATIONS, registerContinuation } from "../src/continuation.ts";
import { PROVIDER_ID } from "../src/provider.ts";

const model = {
	api: "anthropic-messages",
	baseUrl: "http://127.0.0.1/unused",
	contextWindow: 4096,
	cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
	input: ["text"],
	maxTokens: 512,
	name: "Continuation test model",
	provider: PROVIDER_ID,
	id: "fixture",
	reasoning: false,
};

type Reply = { text: string; stopReason: "stop" | "length" | "toolUse" | "error" };

const BROKEN_ANSWER_ERROR = JSON.stringify({
	type: "error",
	error: { type: "api_error", message: "The model returned an answer that could not be used (malformed_function_call). Please retry your request." },
});

function assistantMessage(reply: Reply) {
	return {
		role: "assistant",
		content: reply.stopReason === "toolUse"
			? [{ type: "toolCall", id: `call-${reply.text}`, name: "noop", arguments: {} }]
			: [{ type: "text", text: reply.text }],
		api: model.api,
		provider: model.provider,
		model: model.id,
		usage: { input: 1, output: reply.stopReason === "length" ? model.maxTokens : 1, cacheRead: 0, cacheWrite: 0, totalTokens: 2, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
		stopReason: reply.stopReason,
		...(reply.stopReason === "error" ? { errorMessage: BROKEN_ANSWER_ERROR } : {}),
		timestamp: Date.now(),
	};
}

function streamMessage(message: ReturnType<typeof assistantMessage>) {
	return {
		async *[Symbol.asyncIterator]() {
			yield { type: "start", partial: { ...message, content: [] } };
			if (message.stopReason === "error") yield { type: "error", reason: "error", error: message };
			else yield { type: "done", reason: message.stopReason, message };
		},
		result: async () => message,
	};
}

async function run(replies: Reply[], prompts: string[] = ["fix the bug"]) {
	const cwd = mkdtempSync(join(tmpdir(), "ct-continuation-"));
	const oldAgentDir = process.env.PI_CODING_AGENT_DIR;
	process.env.PI_CODING_AGENT_DIR = join(cwd, "agent");
	const contexts: string[] = [];
	const notices: string[] = [];
	let session: Awaited<ReturnType<typeof createAgentSession>>["session"] | undefined;
	try {
		const settingsManager = SettingsManager.inMemory({ retry: { baseDelayMs: 1 } });
		const resourceLoader = new DefaultResourceLoader({
			cwd,
			agentDir: join(cwd, "agent"),
			settingsManager,
			noExtensions: true,
			noSkills: true,
			noPromptTemplates: true,
			noThemes: true,
			extensionFactories: [{ name: "continuation", factory: (pi) => {
				registerContinuation(pi);
				pi.registerTool({
					name: "noop",
					label: "noop",
					description: "Does nothing.",
					parameters: Type.Object({}),
					execute: async () => ({ content: [{ type: "text" as const, text: "ok" }], details: {} }),
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
				contexts.push(JSON.stringify(context.messages));
				const reply = replies[contexts.length - 1] ?? { text: "Unexpected extra turn", stopReason: "stop" };
				return streamMessage(assistantMessage(reply));
			},
		} as unknown as ModelRuntime;
		const created = await createAgentSession({
			cwd,
			agentDir: join(cwd, "agent"),
			model: model as never,
			modelRuntime,
			settingsManager,
			resourceLoader,
			sessionManager: SessionManager.inMemory(cwd),
			tools: ["noop"],
		});
		session = created.session;
		const host = {
			get session() { return session!; },
			setRebindSession() {},
			async dispose() { session!.dispose(); },
		};
		const write = process.stderr.write.bind(process.stderr);
		process.stderr.write = ((chunk: string | Uint8Array) => {
			notices.push(String(chunk).trim());
			return true;
		}) as typeof process.stderr.write;
		let exitCode: number;
		try {
			exitCode = await runPrintMode(host as never, { mode: "text", initialMessage: prompts[0] });
		} finally {
			process.stderr.write = write;
		}
		const last = [...session.state.messages].reverse().find((message) => message.role === "assistant");
		const finalText = last?.role === "assistant" ? last.content.map((part) => (part.type === "text" ? part.text : "")).join("") : "";
		return { exitCode, finalText, contexts, notices };
	} finally {
		session?.dispose();
		if (oldAgentDir === undefined) delete process.env.PI_CODING_AGENT_DIR;
		else process.env.PI_CODING_AGENT_DIR = oldAgentDir;
		rmSync(cwd, { recursive: true, force: true });
	}
}

test("a reply cut off at the output limit continues the same run", { timeout: 10_000 }, async () => {
	const result = await run([
		{ text: "Step one is done and step two", stopReason: "length" },
		{ text: "Both steps are done.", stopReason: "stop" },
	]);
	assert.equal(result.exitCode, 0);
	assert.equal(result.contexts.length, 2);
	assert.ok(result.contexts[1]!.includes(CONTINUE_PROMPT));
	assert.equal(result.finalText, "Both steps are done.");
	assert.deepEqual(result.notices, []);
});

test("a normal final answer ends the run without another model call", { timeout: 10_000 }, async () => {
	const result = await run([{ text: "Done.", stopReason: "stop" }]);
	assert.equal(result.contexts.length, 1);
	assert.equal(result.finalText, "Done.");
});

test("repeated cut-offs stop after the limit and fail the print run", { timeout: 10_000 }, async () => {
	const cut: Reply = { text: "Still going", stopReason: "length" };
	const result = await run([cut, cut, cut, cut]);
	assert.equal(result.contexts.length, MAX_CONTINUATIONS + 1);
	assert.equal(result.exitCode, 1);
	assert.deepEqual(result.notices, [LIMIT_NOTICE]);
});

test("a reply that is not cut off resets the continue count", { timeout: 10_000 }, async () => {
	const cut: Reply = { text: "Still going", stopReason: "length" };
	const result = await run([
		cut, cut, { text: "a", stopReason: "toolUse" },
		cut, cut, { text: "b", stopReason: "toolUse" },
		{ text: "All done.", stopReason: "stop" },
	]);
	assert.equal(result.contexts.length, 7);
	assert.equal(result.exitCode, 0);
	assert.equal(result.finalText, "All done.");
	assert.deepEqual(result.notices, []);
});

test("a broken answer from the gateway is retried", { timeout: 10_000 }, async () => {
	const result = await run([
		{ text: "", stopReason: "error" },
		{ text: "Recovered.", stopReason: "stop" },
	]);
	assert.equal(result.contexts.length, 2);
	assert.equal(result.exitCode, 0);
	assert.equal(result.finalText, "Recovered.");
});
