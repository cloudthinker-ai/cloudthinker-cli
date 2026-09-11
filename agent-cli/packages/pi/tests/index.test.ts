import assert from "node:assert/strict";
import test from "node:test";

import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";

import { CLOUD_ENTRY_TYPE } from "../src/runtime.ts";
import { CLOUD_TOOLS } from "../src/tools/names.ts";
import cloudthinker from "../src/index.ts";
import { startFakeServer } from "./helpers.ts";

const CONVERSATION = "11111111-1111-4111-8111-111111111111";
const WORKSPACE = "22222222-2222-4222-8222-222222222222";

type Handler = (event: unknown, ctx: ExtensionContext) => unknown;

function settle(ms = 60): Promise<void> {
	return new Promise((resolve) => setTimeout(resolve, ms));
}

test("credits are read once per agent response, the mirror on every event", async () => {
	const savedUrl = process.env.CLOUDTHINKER_URL;
	const savedToken = process.env.CLOUDTHINKER_TOKEN;
	const server = await startFakeServer((request) => {
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
	process.env.CLOUDTHINKER_URL = server.origin;
	process.env.CLOUDTHINKER_TOKEN = "t";
	try {
		const handlers = new Map<string, Handler>();
		let activeTools = ["bash", ...CLOUD_TOOLS];
		const pi = {
			on: (name: string, handler: Handler) => handlers.set(name, handler),
			registerProvider: () => {},
			getActiveTools: () => activeTools,
			setActiveTools: (names: string[]) => { activeTools = names; },
			registerTool: () => {},
			registerCommand: () => {},
			appendEntry: () => {},
			exec: async () => ({ code: 1, stdout: "", stderr: "" }),
		} as unknown as ExtensionAPI;
		await cloudthinker(pi);
		const entries: unknown[] = [];
		const record = (id: string) =>
			entries.push({ type: "message", id, parentId: null, timestamp: "", message: { role: "user", content: id } });
		const ctx = {
			mode: "print",
			hasUI: false,
			cwd: "/tmp/repo",
			sessionManager: { getEntries: () => entries },
			ui: {
				setStatus: () => {},
				setWidget: () => {},
				setTitle: () => {},
				notify: () => {},
			},
		} as unknown as ExtensionContext;
		const creditReads = () =>
			server.requests.filter((request) => request.path.endsWith("/credits")).length;
		const entrySyncs = () =>
			server.requests.filter((request) => request.path.includes("/entries")).length;

		await handlers.get("session_start")?.({ type: "session_start", reason: "new" }, ctx);
		await settle();
		const afterStart = creditReads();
		const syncsAfterStart = entrySyncs();

		for (const [index, name] of ["turn_end", "session_compact", "session_tree", "turn_end"].entries()) {
			record(`${name}-${index}`);
			await handlers.get(name)?.({ type: name }, ctx);
			await settle();
		}
		assert.equal(creditReads(), afterStart);
		assert.equal(entrySyncs(), syncsAfterStart + 4);

		record("agent_end");
		await handlers.get("agent_end")?.({ type: "agent_end" }, ctx);
		await settle();
		assert.equal(creditReads(), afterStart + 1);

		entries.push({ type: "custom", customType: CLOUD_ENTRY_TYPE, data: { enabled: false } });
		await handlers.get("session_start")?.({ type: "session_start", reason: "resume" }, ctx);
		assert.deepEqual(activeTools, ["bash"]);
		const offPrompt = await handlers.get("before_agent_start")?.({ systemPrompt: "base" }, ctx) as { systemPrompt: string };
		assert.match(offPrompt.systemPrompt, /Cloud is off/);
		entries.length = 0;
		await handlers.get("session_start")?.({ type: "session_start", reason: "new" }, ctx);
		assert.deepEqual(activeTools, ["bash", ...CLOUD_TOOLS]);
		await settle();
	} finally {
		if (savedUrl === undefined) delete process.env.CLOUDTHINKER_URL;
		else process.env.CLOUDTHINKER_URL = savedUrl;
		if (savedToken === undefined) delete process.env.CLOUDTHINKER_TOKEN;
		else process.env.CLOUDTHINKER_TOKEN = savedToken;
		await server.close();
	}
});
