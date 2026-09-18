import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";

import { CLOUD_ENTRY_TYPE, CloudThinkerRuntime, SESSION_ENTRY_TYPE } from "../src/runtime.ts";
import { machineBarLines, TAG_LEGEND } from "../src/awareness.ts";
import { CONVERSATION_HEADER } from "../src/provider.ts";
import type { CloudThinkerClient } from "../src/client.ts";
import { CLOUD_TOOLS } from "../src/tools/names.ts";
import cloudthinker from "../src/index.ts";
import { startFakeServer } from "./helpers.ts";

const CONVERSATION = "11111111-1111-4111-8111-111111111111";
const WORKSPACE = "22222222-2222-4222-8222-222222222222";

type Handler = (event: unknown, ctx: ExtensionContext) => unknown;

function settle(ms = 60): Promise<void> {
	return new Promise((resolve) => setTimeout(resolve, ms));
}

async function until(predicate: () => boolean, timeoutMs = 3_000): Promise<void> {
	const deadline = Date.now() + timeoutMs;
	while (!predicate() && Date.now() < deadline) await settle(20);
}

function startCloudServer() {
	return startFakeServer((request) => {
		if (request.path === "/api/v1/agent-cli/models") return { body: { models: [] } };
		if (request.path === "/api/v1/agent-cli/sessions" && request.method === "POST") {
			return {
				body: {
					conversation_id: CONVERSATION,
					workspace_id: WORKSPACE,
					web_url: "http://web/c-1",
					auto_mode: { enabled: false, can_edit: false },
				},
			};
		}
		if (request.path === "/api/v1/cli/whoami") {
			return {
				body: {
					user_email: "dev@acme.io",
					workspace_id: WORKSPACE,
					workspace_name: "acme-prod",
					organization_id: null,
				},
			};
		}
		if (request.path === "/api/v1/agent-cli/connections") return { body: { xml: "", prefixes: [] } };
		if (request.path.endsWith("/credits")) return { body: { credits_used: 1, tokens_consumed: 10 } };
		if (request.path.includes("/entries") && request.method === "GET") {
			return { body: { entries: [], last_seq: 0 } };
		}
		if (request.path.includes("/entries")) return { body: { stored: 0, last_seq: 0 } };
		return undefined;
	});
}

function piHarness() {
	const agentDir = mkdtempSync(join(tmpdir(), "ct-pi-agent-dir-"));
	const savedAgentDir = process.env.PI_CODING_AGENT_DIR;
	process.env.PI_CODING_AGENT_DIR = join(agentDir, "agent");
	const handlers = new Map<string, Handler>();
	const commands = new Map<string, { handler: (args: string, ctx: ExtensionContext) => Promise<void> }>();
	let activeTools = ["bash", ...CLOUD_TOOLS];
	const pi = {
		on: (name: string, handler: Handler) => handlers.set(name, handler),
		registerProvider: () => {},
		getActiveTools: () => activeTools,
		setActiveTools: (names: string[]) => { activeTools = names; },
		registerTool: () => {},
		registerCommand: (name: string, command: { handler: (args: string, ctx: ExtensionContext) => Promise<void> }) => commands.set(name, command),
		appendEntry: () => {},
		exec: async () => ({ code: 1, stdout: "", stderr: "" }),
	} as unknown as ExtensionAPI;
	return {
		pi,
		handlers,
		commands,
		activeTools: () => activeTools,
		dispose: () => {
			if (savedAgentDir === undefined) delete process.env.PI_CODING_AGENT_DIR;
			else process.env.PI_CODING_AGENT_DIR = savedAgentDir;
			rmSync(agentDir, { recursive: true, force: true });
		},
	};
}

function sessionContext(cwd: string, entries: unknown[], options: { mode?: string; notify?: (message: string) => void; projectTrusted?: boolean; provider?: string } = {}): ExtensionContext {
	return {
		mode: options.mode ?? "print",
		hasUI: false,
		cwd,
		isIdle: () => true,
		isProjectTrusted: () => options.projectTrusted ?? true,
		...(options.provider ? { model: { provider: options.provider } } : {}),
		sessionManager: { getEntries: () => entries },
		ui: {
			setStatus: () => {},
			setWidget: () => {},
			setTitle: () => {},
			setHeader: () => {},
			notify: options.notify ?? (() => {}),
		},
	} as unknown as ExtensionContext;
}

test("credits are read once per agent response, the mirror on every event", async () => {
	const savedUrl = process.env.CLOUDTHINKER_URL;
	const savedToken = process.env.CLOUDTHINKER_TOKEN;
	const server = await startCloudServer();
	const { pi, handlers, activeTools, dispose } = piHarness();
	process.env.CLOUDTHINKER_URL = server.origin;
	process.env.CLOUDTHINKER_TOKEN = "t";
	try {
		await cloudthinker(pi);
		const entries: unknown[] = [];
		const record = (id: string) =>
			entries.push({ type: "message", id, parentId: null, timestamp: "", message: { role: "user", content: id } });
		const ctx = sessionContext("/tmp/repo", entries);
		const creditReads = () =>
			server.requests.filter((request) => request.path.endsWith("/credits")).length;
		const delivered = () =>
			server.requests
				.filter((request) => request.path.includes("/entries") && request.method === "PUT")
				.flatMap((request) => (request.body as { entries?: { entry_id?: string }[] } | undefined)?.entries ?? [])
				.map((entry) => entry.entry_id);

		await handlers.get("session_start")?.({ type: "session_start", reason: "new" }, ctx);
		await settle();
		const afterStart = creditReads();

		const mirrored: string[] = [];
		for (const [index, name] of ["turn_end", "session_compact", "session_tree", "turn_end"].entries()) {
			const id = `${name}-${index}`;
			mirrored.push(id);
			record(id);
			await handlers.get(name)?.({ type: name }, ctx);
			await settle();
		}
		await until(() => mirrored.every((id) => delivered().includes(id)));
		for (const id of mirrored) assert.ok(delivered().includes(id), `the mirror missed ${id}`);
		assert.equal(creditReads(), afterStart);

		record("agent_end");
		await handlers.get("agent_end")?.({ type: "agent_end" }, ctx);
		await until(() => creditReads() === afterStart + 1);
		assert.equal(creditReads(), afterStart + 1);

		entries.push({ type: "custom", customType: CLOUD_ENTRY_TYPE, data: { enabled: false } });
		await handlers.get("session_start")?.({ type: "session_start", reason: "resume" }, ctx);
		assert.deepEqual(activeTools(), ["bash"]);
		const offPrompt = await handlers.get("before_agent_start")?.({ systemPrompt: "base" }, ctx) as { systemPrompt: string };
		assert.match(offPrompt.systemPrompt, /Cloud is off/);
		entries.length = 0;
		await handlers.get("session_start")?.({ type: "session_start", reason: "new" }, ctx);
		assert.deepEqual(activeTools(), ["bash", ...CLOUD_TOOLS]);
		await settle();
	} finally {
		if (savedUrl === undefined) delete process.env.CLOUDTHINKER_URL;
		else process.env.CLOUDTHINKER_URL = savedUrl;
		if (savedToken === undefined) delete process.env.CLOUDTHINKER_TOKEN;
		else process.env.CLOUDTHINKER_TOKEN = savedToken;
		dispose();
		await server.close();
	}
});

test("CA-CLOUD-9: a new session takes cloudDefault, while a recorded choice still wins", async () => {
	const savedUrl = process.env.CLOUDTHINKER_URL;
	const savedToken = process.env.CLOUDTHINKER_TOKEN;
	const root = mkdtempSync(join(tmpdir(), "ct-cloud-default-"));
	const server = await startCloudServer();
	const { pi, handlers, activeTools, dispose } = piHarness();
	process.env.CLOUDTHINKER_URL = server.origin;
	process.env.CLOUDTHINKER_TOKEN = "t";
	mkdirSync(join(root, ".pi"), { recursive: true });
	writeFileSync(join(root, ".pi", "settings.json"), JSON.stringify({ cloudDefault: false }));
	try {
		await cloudthinker(pi);
		const entries: unknown[] = [];
		const ctx = sessionContext(root, entries);

		await handlers.get("session_start")?.({ type: "session_start", reason: "new" }, ctx);
		assert.deepEqual(activeTools(), ["bash"]);
		const offPrompt = await handlers.get("before_agent_start")?.({ systemPrompt: "base" }, ctx) as { systemPrompt: string };
		assert.match(offPrompt.systemPrompt, /Cloud is off/);

		entries.push({ type: "custom", customType: CLOUD_ENTRY_TYPE, data: { enabled: true } });
		await handlers.get("session_start")?.({ type: "session_start", reason: "resume" }, ctx);
		assert.deepEqual(activeTools(), ["bash", ...CLOUD_TOOLS]);

		entries.length = 0;
		rmSync(join(root, ".pi", "settings.json"));
		await handlers.get("session_start")?.({ type: "session_start", reason: "new" }, ctx);
		assert.deepEqual(activeTools(), ["bash", ...CLOUD_TOOLS]);
		await settle();
	} finally {
		if (savedUrl === undefined) delete process.env.CLOUDTHINKER_URL;
		else process.env.CLOUDTHINKER_URL = savedUrl;
		if (savedToken === undefined) delete process.env.CLOUDTHINKER_TOKEN;
		else process.env.CLOUDTHINKER_TOKEN = savedToken;
		dispose();
		rmSync(root, { recursive: true, force: true });
		await server.close();
	}
});

test("CA-AWARE-9: a local missing-path error names this machine and points at the sandbox, once", async () => {
	const savedUrl = process.env.CLOUDTHINKER_URL;
	const savedToken = process.env.CLOUDTHINKER_TOKEN;
	const server = await startCloudServer();
	const { pi, handlers, dispose } = piHarness();
	process.env.CLOUDTHINKER_URL = server.origin;
	process.env.CLOUDTHINKER_TOKEN = "t";
	try {
		await cloudthinker(pi);
		const ctx = sessionContext("/tmp/repo", []);
		const missing = { type: "tool_result", toolCallId: "t", toolName: "read", input: {}, content: [{ type: "text", text: "File not found: /tmp/repo/missing.ts" }], isError: true };
		const result = await handlers.get("tool_result")?.(missing, ctx) as { content: { text: string }[] } | undefined;
		assert.match(result?.content.map((block) => block.text).join("\n") ?? "", /looked for on this machine/);
		const shell = await handlers.get("tool_result")?.({ ...missing, toolName: "powershell" }, ctx) as { content: { text: string }[] } | undefined;
		assert.match(shell?.content.map((block) => block.text).join("\n") ?? "", /looked for on this machine/);
		assert.equal(await handlers.get("tool_result")?.({ ...missing, isError: false }, ctx), undefined);
		assert.equal(await handlers.get("tool_result")?.({ ...missing, toolName: "ct_sandbox_read" }, ctx), undefined);
		assert.equal(await handlers.get("tool_result")?.({ ...missing, content: [{ type: "text", text: "exit code 1" }] }, ctx), undefined);
	} finally {
		if (savedUrl === undefined) delete process.env.CLOUDTHINKER_URL;
		else process.env.CLOUDTHINKER_URL = savedUrl;
		if (savedToken === undefined) delete process.env.CLOUDTHINKER_TOKEN;
		else process.env.CLOUDTHINKER_TOKEN = savedToken;
		dispose();
		await server.close();
	}
});

test("CA-AWARE-10: a brand-new TUI session offers the tour in one line and never runs it", async () => {
	const savedUrl = process.env.CLOUDTHINKER_URL;
	const savedToken = process.env.CLOUDTHINKER_TOKEN;
	const server = await startCloudServer();
	const { pi, handlers, dispose } = piHarness();
	process.env.CLOUDTHINKER_URL = server.origin;
	process.env.CLOUDTHINKER_TOKEN = "t";
	const OFFER = "New here? /tour shows which machine runs what";
	const count = (notices: string[]) => notices.filter((message) => message === OFFER).length;
	const history = [{ type: "message", id: "m1", parentId: null, timestamp: "", message: { role: "user", content: "hi" } }];
	try {
		await cloudthinker(pi);
		const entries: unknown[] = [];
		const notices: string[] = [];
		const ctx = sessionContext("/tmp/repo", entries, { mode: "tui", notify: (message) => notices.push(message) });
		await handlers.get("session_start")?.({ type: "session_start", reason: "startup" }, ctx);
		await settle();
		assert.equal(count(notices), 1);
		assert.equal(server.requests.some((request) => JSON.stringify(request.body ?? "").includes("uname")), false);
		entries.push(...history);
		await handlers.get("session_start")?.({ type: "session_start", reason: "new" }, ctx);
		await settle();
		assert.equal(count(notices), 1);
		const continued: string[] = [];
		await handlers.get("session_start")?.({ type: "session_start", reason: "startup" }, sessionContext("/tmp/repo", history, { mode: "tui", notify: (message) => continued.push(message) }));
		await settle();
		assert.equal(count(continued), 0);
		const resumed: string[] = [];
		await handlers.get("session_start")?.({ type: "session_start", reason: "resume" }, sessionContext("/tmp/repo", history, { mode: "tui", notify: (message) => resumed.push(message) }));
		await settle();
		assert.equal(count(resumed), 0);
		const forked: string[] = [];
		await handlers.get("session_start")?.({ type: "session_start", reason: "fork" }, sessionContext("/tmp/repo", history, { mode: "tui", notify: (message) => forked.push(message) }));
		await settle();
		assert.equal(count(forked), 0);
		const renewed: string[] = [];
		await handlers.get("session_start")?.({ type: "session_start", reason: "new" }, sessionContext("/tmp/repo", [], { mode: "tui", notify: (message) => renewed.push(message) }));
		await settle();
		assert.equal(count(renewed), 1);
	} finally {
		if (savedUrl === undefined) delete process.env.CLOUDTHINKER_URL;
		else process.env.CLOUDTHINKER_URL = savedUrl;
		if (savedToken === undefined) delete process.env.CLOUDTHINKER_TOKEN;
		else process.env.CLOUDTHINKER_TOKEN = savedToken;
		dispose();
		await server.close();
	}
});

test("a delegated child's Cloud state leaves the startup bar on the parent's session", async () => {
	const savedUrl = process.env.CLOUDTHINKER_URL;
	const savedToken = process.env.CLOUDTHINKER_TOKEN;
	const server = await startCloudServer();
	const parent = piHarness();
	const child = piHarness();
	process.env.CLOUDTHINKER_URL = server.origin;
	process.env.CLOUDTHINKER_TOKEN = "t";
	try {
		await cloudthinker(parent.pi);
		await cloudthinker(child.pi, { cloudEnabled: false, sourceConversationId: CONVERSATION });
		await parent.handlers.get("session_start")?.({ type: "session_start", reason: "startup" }, sessionContext("/tmp/repo", [], { mode: "tui" }));
		await settle();
		await child.handlers.get("session_start")?.({ type: "session_start", reason: "startup" }, sessionContext("/tmp/child-repo", [], { mode: "tui" }));
		await settle();
		const style = { local: (text: string) => text, cloud: (text: string) => text };
		const lines = machineBarLines(style);
		assert.match(lines[0]!, /\/tmp\/repo/);
		assert.match(lines.at(-1)!, /Cloud: On/);
	} finally {
		if (savedUrl === undefined) delete process.env.CLOUDTHINKER_URL;
		else process.env.CLOUDTHINKER_URL = savedUrl;
		if (savedToken === undefined) delete process.env.CLOUDTHINKER_TOKEN;
		else process.env.CLOUDTHINKER_TOKEN = savedToken;
		parent.dispose();
		child.dispose();
		await server.close();
	}
});

test("CA-CLOUD-10: cloud off by default starts nothing remote, and /cloud on initializes the session", async () => {
	const savedUrl = process.env.CLOUDTHINKER_URL;
	const savedToken = process.env.CLOUDTHINKER_TOKEN;
	const root = mkdtempSync(join(tmpdir(), "ct-cloud-off-start-"));
	const server = await startCloudServer();
	const { pi, handlers, commands, dispose } = piHarness();
	process.env.CLOUDTHINKER_URL = server.origin;
	process.env.CLOUDTHINKER_TOKEN = "t";
	mkdirSync(join(root, ".pi"), { recursive: true });
	writeFileSync(join(root, ".pi", "settings.json"), JSON.stringify({ cloudDefault: false }));
	const remote = () =>
		server.requests.filter((request) =>
			(request.path === "/api/v1/agent-cli/sessions" && request.method === "POST")
			|| request.path === "/api/v1/cli/whoami"
			|| request.path === "/api/v1/agent-cli/connections",
		);
	try {
		await cloudthinker(pi);
		const entries: unknown[] = [];
		const ctx = sessionContext(root, entries);

		await handlers.get("session_start")?.({ type: "session_start", reason: "new" }, ctx);
		await settle();
		assert.deepEqual(remote(), []);

		await commands.get("cloud")!.handler("on", ctx);
		await until(() => remote().length >= 3);
		assert.ok(remote().some((request) => request.path === "/api/v1/agent-cli/sessions"));
		assert.ok(remote().some((request) => request.path === "/api/v1/cli/whoami"));
		assert.ok(remote().some((request) => request.path === "/api/v1/agent-cli/connections"));
		await settle();
	} finally {
		if (savedUrl === undefined) delete process.env.CLOUDTHINKER_URL;
		else process.env.CLOUDTHINKER_URL = savedUrl;
		if (savedToken === undefined) delete process.env.CLOUDTHINKER_TOKEN;
		else process.env.CLOUDTHINKER_TOKEN = savedToken;
		dispose();
		rmSync(root, { recursive: true, force: true });
		await server.close();
	}
});

test("CA-CLOUD-11: a Cloud-off session links lazily on its first turn, once, and reuses what a resume recorded", async () => {
	const savedUrl = process.env.CLOUDTHINKER_URL;
	const savedToken = process.env.CLOUDTHINKER_TOKEN;
	const root = mkdtempSync(join(tmpdir(), "ct-cloud-lazy-"));
	const server = await startCloudServer();
	const { pi, handlers, commands, dispose } = piHarness();
	process.env.CLOUDTHINKER_URL = server.origin;
	process.env.CLOUDTHINKER_TOKEN = "t";
	mkdirSync(join(root, ".pi"), { recursive: true });
	writeFileSync(join(root, ".pi", "settings.json"), JSON.stringify({ cloudDefault: false }));
	const created = () =>
		server.requests.filter((request) => request.path === "/api/v1/agent-cli/sessions" && request.method === "POST");
	const parentConversation = "99999999-9999-4999-8999-999999999999";
	const sessionEntry = { type: "custom", id: "s-1", parentId: null, timestamp: "", customType: SESSION_ENTRY_TYPE, data: { conversation_id: parentConversation, workspace_id: WORKSPACE, web_url: "http://web/parent", auto_mode: { enabled: false, can_edit: false } } };
	try {
		await cloudthinker(pi);
		const fresh = sessionContext(root, []);
		await handlers.get("session_start")?.({ type: "session_start", reason: "new" }, fresh);
		await settle();
		assert.equal(created().length, 0);

		await handlers.get("before_agent_start")?.({ systemPrompt: "base" }, sessionContext(root, [], { provider: "cloudthinker" }));
		await until(() => created().length === 1);
		assert.equal(created().length, 1);
		const headers: Record<string, string | null> = {};
		await handlers.get("before_provider_headers")?.({ headers }, sessionContext(root, [], { provider: "cloudthinker" }));
		assert.equal(headers[CONVERSATION_HEADER], CONVERSATION,
			"the first turn must carry the conversation the gateway needs");
		await handlers.get("before_agent_start")?.({ systemPrompt: "base" }, sessionContext(root, [], { provider: "cloudthinker" }));
		await settle();
		assert.equal(created().length, 1, "the lazy link runs once");

		const resumed = sessionContext(root, [sessionEntry], { provider: "cloudthinker" });
		await handlers.get("session_start")?.({ type: "session_start", reason: "resume" }, resumed);
		await handlers.get("before_agent_start")?.({ systemPrompt: "base" }, resumed);
		await settle();
		assert.equal(created().length, 1, "a resume reuses its own recorded conversation");
		const resumedHeaders: Record<string, string | null> = {};
		await handlers.get("before_provider_headers")?.({ headers: resumedHeaders }, resumed);
		assert.equal(resumedHeaders[CONVERSATION_HEADER], parentConversation);

		const fork = sessionContext(root, [sessionEntry], { provider: "cloudthinker" });
		await handlers.get("session_start")?.({ type: "session_start", reason: "fork" }, fork);
		await commands.get("cloud")!.handler("on", fork);
		await until(() => created().length === 2);
		assert.equal(created().length, 2, "/cloud on opens the fork's own conversation");
		assert.equal((created()[1]!.body as { source_conversation_id?: string }).source_conversation_id, parentConversation);
		const forkHeaders: Record<string, string | null> = {};
		await handlers.get("before_provider_headers")?.({ headers: forkHeaders }, fork);
		assert.equal(forkHeaders[CONVERSATION_HEADER], CONVERSATION, "the fork never adopts the parent conversation");
		await settle();
	} finally {
		if (savedUrl === undefined) delete process.env.CLOUDTHINKER_URL;
		else process.env.CLOUDTHINKER_URL = savedUrl;
		if (savedToken === undefined) delete process.env.CLOUDTHINKER_TOKEN;
		else process.env.CLOUDTHINKER_TOKEN = savedToken;
		dispose();
		rmSync(root, { recursive: true, force: true });
		await server.close();
	}
});

test("CA-AWARE-11: each session runtime owns its legend, so a child never claims the parent's", async () => {
	const { pi } = piHarness();
	const parent = new CloudThinkerRuntime(pi, {} as CloudThinkerClient);
	const child = new CloudThinkerRuntime(pi, {} as CloudThinkerClient);
	assert.equal(parent.legend.tag("parent-1"), TAG_LEGEND);
	assert.equal(child.legend.tag("child-1"), TAG_LEGEND, "the child draws its own legend");
	assert.equal(parent.legend.tag("parent-2"), undefined, "the child never claims the parent's owner");
	child.reset();
	assert.equal(parent.legend.tag("parent-2"), undefined, "a child reset leaves the parent's owner alone");
	parent.reset();
	assert.equal(parent.legend.tag("parent-3"), TAG_LEGEND);
});
