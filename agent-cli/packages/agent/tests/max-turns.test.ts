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

import { extractMaxTurns, maxTurnsLine, registerMaxTurns } from "../src/max-turns.ts";

const model = {
	api: "openai-completions",
	baseUrl: "http://127.0.0.1/unused",
	contextWindow: 4096,
	cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
	input: ["text"],
	maxTokens: 512,
	name: "Max turns test model",
	provider: "max-turns-test",
	id: "fixture",
	reasoning: false,
};

type Part = { type: "text"; text: string } | { type: "toolCall"; id: string; name: string; arguments: { path: string } };

function reply(content: Part[], stopReason: "stop" | "toolUse") {
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

function stream(message: ReturnType<typeof reply>, aborted: boolean) {
	const final = aborted ? { ...message, content: [], stopReason: "aborted" } : message;
	return {
		async *[Symbol.asyncIterator]() {
			yield { type: "start", partial: { ...final, content: [] } };
			yield aborted ? { type: "error", reason: "aborted", error: final } : { type: "done", reason: message.stopReason, message };
		},
		result: async () => final,
	};
}

async function headlessRun(mode: "text" | "json", limit: number, toolTurns: number) {
	const cwd = mkdtempSync(join(tmpdir(), "ct-max-turns-"));
	const agentDir = join(cwd, "agent");
	const settingsManager = SettingsManager.inMemory({});
	let calls = 0;
	const stdout: string[] = [];
	const stderr: string[] = [];
	const writeOut = process.stdout.write;
	const writeErr = process.stderr.write;
	const logError = console.error;
	const previousExit = process.exitCode;
	process.exitCode = undefined;
	try {
		const resourceLoader = new DefaultResourceLoader({
			cwd,
			agentDir,
			settingsManager,
			noExtensions: true,
			noSkills: true,
			noPromptTemplates: true,
			noThemes: true,
			extensionFactories: [{ name: "max-turns", factory: (pi) => { registerMaxTurns(pi, limit); } }],
		});
		await resourceLoader.reload();
		const modelRuntime = {
			hasConfiguredAuth: () => true,
			checkAuth: async () => ({ type: "api_key", source: "test" }),
			getAuth: async () => ({ auth: { apiKey: "test" }, env: {} }),
			isUsingOAuth: () => false,
			getModel: (provider: string, id: string) => (provider === model.provider && id === model.id ? model : undefined),
			streamSimple: (_model: unknown, _context: unknown, options: { signal?: AbortSignal } | undefined) => {
				const aborted = options?.signal?.aborted === true;
				if (!aborted) calls += 1;
				return stream(calls <= toolTurns
					? reply([{ type: "toolCall", id: `call-${calls}`, name: "ls", arguments: { path: "." } }], "toolUse")
					: reply([{ type: "text", text: "All done" }], "stop"), aborted);
			},
		} as unknown as ModelRuntime;
		const { session } = await createAgentSession({
			cwd,
			agentDir,
			model: model as never,
			modelRuntime,
			settingsManager,
			resourceLoader,
			sessionManager: SessionManager.create(cwd),
			tools: ["ls"],
		});
		const host = {
			get session() { return session; },
			setRebindSession() {},
			async dispose() {
				await session.extensionRunner.emit({ type: "session_shutdown", reason: "quit" });
				session.dispose();
			},
		};
		process.stdout.write = ((chunk: string | Uint8Array, ...rest: unknown[]) => {
			if (typeof chunk !== "string" || !chunk.startsWith("{")) return writeOut.call(process.stdout, chunk, ...(rest as []));
			stdout.push(chunk);
			for (const arg of rest) if (typeof arg === "function") arg();
			return true;
		}) as typeof process.stdout.write;
		process.stderr.write = ((chunk: string | Uint8Array, ...rest: unknown[]) => {
			stderr.push(String(chunk));
			for (const arg of rest) if (typeof arg === "function") arg();
			return true;
		}) as typeof process.stderr.write;
		console.error = (...args: unknown[]) => { stderr.push(`${args.join(" ")}\n`); };
		const printed = await runPrintMode(host as never, { mode, initialMessage: "go" });
		const exitCode = process.exitCode ?? printed;
		return { calls, exitCode, stdout: stdout.join(""), stderr: stderr.join("") };
	} finally {
		process.stdout.write = writeOut;
		process.stderr.write = writeErr;
		console.error = logError;
		process.exitCode = previousExit;
		rmSync(cwd, { recursive: true, force: true });
	}
}

test("--max-turns is read from argv and removed before pi sees it", () => {
	assert.deepEqual(extractMaxTurns(["-p", "hi", "--max-turns", "3"]), { limit: 3, rest: ["-p", "hi"] });
	assert.deepEqual(extractMaxTurns(["-p", "hi"]), { limit: undefined, rest: ["-p", "hi"] });
});

test("a text run that would continue past the cap stops, names the cap on stderr once, and exits 1", { timeout: 10_000 }, async () => {
	const result = await headlessRun("text", 2, 10);
	assert.equal(result.calls, 2);
	assert.equal(result.exitCode, 1);
	assert.equal(result.stderr, `${maxTurnsLine(2)}\n`);
});

test("a json run emits a max_turns event, names the cap on stderr, and exits 1", { timeout: 10_000 }, async () => {
	const result = await headlessRun("json", 2, 10);
	assert.equal(result.exitCode, 1);
	assert.equal(result.stderr, `${maxTurnsLine(2)}\n`);
	const events = result.stdout.split("\n").filter(Boolean).map((line) => JSON.parse(line) as { type: string; maxTurns?: number });
	assert.deepEqual(events.filter((event) => event.type === "max_turns"), [{ type: "max_turns", maxTurns: 2 }]);
});

test("a run that finishes within the cap exits 0 with nothing on stderr", { timeout: 10_000 }, async () => {
	const result = await headlessRun("text", 2, 1);
	assert.equal(result.calls, 2);
	assert.equal(result.exitCode, 0);
	assert.equal(result.stderr, "");
});
