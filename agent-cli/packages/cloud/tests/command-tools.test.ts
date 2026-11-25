import assert from "node:assert/strict";
import test from "node:test";

import type { ExtensionAPI, ToolDefinition } from "@earendil-works/pi-coding-agent";

import { CloudThinkerClient, TokenSource } from "../src/client.ts";
import { hostVersionsFrom } from "../src/versions.ts";
import { CloudThinkerRuntime } from "../src/runtime.ts";
import { registerCommandTools } from "../src/tools/command-tools.ts";
import { registerConnectionDiscovery } from "../src/tools/connection-discovery.ts";
import { CLOUD_TOOLS, CT_ASK, LOAD_SKILL, TOOL_SEARCH } from "../src/tools/names.ts";
import { type Route, startFakeServer } from "./helpers.ts";

async function harness(route: Route) {
	const server = await startFakeServer(route);
	const tools = new Map<string, ToolDefinition>();
	let active: string[] = ["bash"];
	const pi = {
		registerTool: (tool: ToolDefinition) => {
			tools.set(tool.name, tool);
			active = [...new Set([...active, tool.name])];
		},
		getActiveTools: () => active,
		setActiveTools: (names: string[]) => {
			active = names;
		},
	} as unknown as ExtensionAPI;
	const client = new CloudThinkerClient({
		baseUrl: server.origin,
		tokens: new TokenSource({ CLOUDTHINKER_TOKEN: "t" }),
	});
	const runtime = new CloudThinkerRuntime(pi, client, hostVersionsFrom({ version: "0.4.0", piVersion: "0.85.1" }, "0.4.0"));
	runtime.session = {
		conversation_id: "c-1",
		workspace_id: "w-1",
		web_url: "https://web/c-1",
		auto_mode: { enabled: false, can_edit: false },
	};
	return { server, tools, runtime, active: () => active };
}

const manifest = {
	tools: [
		{ name: "list_incidents", title: "List incidents", description: "List incidents.\nMore.", kind: "read", input_schema: { type: "object", properties: {} } },
		{ name: "ask_cloudthinker", title: "Ask", description: "Start a run.", kind: "run", input_schema: { type: "object", properties: {} } },
		{ name: CT_ASK, title: "Shadow", description: "Must not replace the hand-written tool.", kind: "read", input_schema: { type: "object", properties: {} } },
	],
};

test("manifest read tools register as cloud tools and answer through the backend", async () => {
	const h = await harness((request) => {
		if (request.method === "GET" && request.path === "/api/v1/agent-cli/tools") return { body: manifest };
		if (request.path === "/api/v1/agent-cli/tools/list_incidents") {
			const days = (request.body as { arguments: { days?: number } }).arguments.days;
			if (days === 0) {
				return { body: { text: "INVALID_ARGUMENTS: Invalid arguments: days.\nNext: Fix it.", error: { code: "INVALID_ARGUMENTS", message: "Invalid arguments: days." } } };
			}
			return { body: { text: "1 of 1 incidents.", structured: { total: 1 }, error: null } };
		}
		return undefined;
	});
	try {
		assert.deepEqual(await registerCommandTools(h.runtime), ["list_incidents"]);
		assert.deepEqual([...h.tools.keys()], ["list_incidents"]);
		assert.ok(CLOUD_TOOLS.includes("list_incidents"));

		const tool = h.tools.get("list_incidents")!;
		const ok = await tool.execute("call-1", {}, undefined, undefined, undefined as never);
		assert.equal((ok.content[0] as { text: string }).text, "1 of 1 incidents.");
		await assert.rejects(
			tool.execute("call-2", { days: 0 }, undefined, undefined, undefined as never),
			/INVALID_ARGUMENTS: Invalid arguments: days/,
		);

		h.runtime.setCloudEnabled(false, false);
		assert.ok(!h.active().includes("list_incidents"));
	} finally {
		await h.server.close();
	}
});

test("a backend without the tools route keeps the hand-written tools only", async () => {
	const h = await harness(() => undefined);
	try {
		assert.deepEqual(await registerCommandTools(h.runtime), []);
		assert.equal(h.tools.size, 0);
	} finally {
		await h.server.close();
	}
});

test("Connection discovery loads a tool and its policy without running either", async () => {
	const schema = '<tool_schema name="cloudflare.docs">Search Cloudflare documentation.</tool_schema>';
	const policy = "no-approval tools: docs\napproval-required tools: execute";
	const h = await harness((request) => {
		if (request.method !== "POST") return undefined;
		if (request.path === "/api/v1/agent-cli/tools/search") {
			return { body: { content: schema, mode: "names", hit_count: 1 } };
		}
		if (request.path === "/api/v1/agent-cli/skills/load") {
			return { body: { content: policy, roster_only: true } };
		}
		return undefined;
	});
	try {
		registerConnectionDiscovery(h.runtime);
		const search = h.tools.get(TOOL_SEARCH)!;
		const load = h.tools.get(LOAD_SKILL)!;
		const found = await search.execute("search", { query: "cloudflare.docs" }, undefined, undefined, undefined as never);
		const loaded = await load.execute("load", { connection_prefix: "cloudflare" }, undefined, undefined, undefined as never);
		assert.deepEqual(found.content, [{ type: "text", text: schema }]);
		assert.deepEqual(loaded.content, [{ type: "text", text: policy }]);

		h.runtime.setCloudEnabled(false, false);
		assert.ok(!h.active().includes(TOOL_SEARCH));
		assert.ok(!h.active().includes(LOAD_SKILL));
		await assert.rejects(
			load.execute("off", { connection_prefix: "cloudflare" }, undefined, undefined, undefined as never),
			/Cloud is off/,
		);
		assert.deepEqual(h.server.requests.map(({ method, path, body }) => ({ method, path, body })), [
			{ method: "POST", path: "/api/v1/agent-cli/tools/search", body: { conversation_id: "c-1", query: "cloudflare.docs" } },
			{ method: "POST", path: "/api/v1/agent-cli/skills/load", body: { conversation_id: "c-1", connection_prefix: "cloudflare" } },
		]);
	} finally {
		await h.server.close();
	}
});
