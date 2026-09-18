import assert from "node:assert/strict";
import test from "node:test";
import type { ExtensionAPI, ExtensionCommandContext, ToolDefinition } from "@earendil-works/pi-coding-agent";
import type { CloudThinkerClient } from "../src/client.ts";
import { registerCommands } from "../src/commands.ts";
import { buildPromptBlock } from "../src/prompt.ts";
import { CLOUD_ENTRY_TYPE, CloudThinkerRuntime } from "../src/runtime.ts";
import { CLOUD_TOOLS } from "../src/tools/names.ts";
import { registerSandboxRead } from "../src/tools/ct-sandbox-read.ts";
import { registerSandboxWrite } from "../src/tools/ct-sandbox-write.ts";
import { registerAsk } from "../src/tools/ct-ask.ts";
import { registerRunStatus } from "../src/tools/ct-run-status.ts";
import { registerReadTaskOutput } from "../src/tools/read-task-output.ts";

function harness() {
	let active = ["bash", "read", "write", "edit", ...CLOUD_TOOLS];
	const entries: { customType: string; data: unknown }[] = [];
	const tools = new Map<string, ToolDefinition>();
	const commands = new Map<string, { handler: (args: string, ctx: ExtensionCommandContext) => Promise<void> }>();
	const messages: string[] = [];
	const statuses: string[] = [];
	let idle = true;
	const pi = {
		getActiveTools: () => active,
		setActiveTools: (names: string[]) => { active = names; },
		appendEntry: (customType: string, data: unknown) => entries.push({ customType, data }),
		registerTool: (tool: ToolDefinition) => tools.set(tool.name, tool),
		registerCommand: (name: string, command: { handler: (args: string, ctx: ExtensionCommandContext) => Promise<void> }) => commands.set(name, command),
	} as unknown as ExtensionAPI;
	const clientCalls: string[] = [];
	const client = {
		createSession: async () => {
			clientCalls.push("createSession");
			return { conversation_id: "c-1", workspace_id: "w-1", web_url: "https://web/c-1", auto_mode: { enabled: false, can_edit: false } };
		},
		whoami: async () => {
			clientCalls.push("whoami");
			return { user_email: "dev@acme.io", workspace_id: "w-1", workspace_name: "acme-prod", organization_id: null };
		},
		getConnectionsContext: async () => {
			clientCalls.push("connections");
			return { xml: "", prefixes: [] };
		},
	} as unknown as CloudThinkerClient;
	const runtime = new CloudThinkerRuntime(pi, client);
	const ctx = {
		isIdle: () => idle,
		sessionManager: { getEntries: () => entries },
		ui: { notify: (message: string) => messages.push(message), setStatus: (_key: string, text?: string) => { if (text) statuses.push(text); } },
	} as unknown as ExtensionCommandContext;
	registerCommands(runtime);
	return { runtime, tools, entries, messages, statuses, clientCalls, active: () => active, setIdle: (value: boolean) => { idle = value; }, command: (args: string) => commands.get("cloud")!.handler(args, ctx), ctx };
}

test("CA-CLOUD-1 off hides cloud tools and on preserves local tool changes", async () => {
	const h = harness();
	await h.command("off");
	assert.deepEqual(h.active(), ["bash", "read", "write", "edit"]);
	assert.equal(h.runtime.cloudEnabled, false);
	assert.deepEqual(h.entries.at(-1), { customType: CLOUD_ENTRY_TYPE, data: { enabled: false } });
	assert.equal(h.statuses.at(-1), "[L]");
	h.runtime.pi.setActiveTools(["read", "custom_local"]);
	await h.command("off");
	await h.command("on");
	assert.deepEqual(h.active(), ["read", "custom_local", ...CLOUD_TOOLS]);
	assert.equal(h.statuses.at(-1), "[L+C]");
	assert.match(h.messages.at(-1)!, /Cloud: On/);
});

test("CA-CLOUD-2 invalid arguments and an active turn leave the choice unchanged", async () => {
	const h = harness();
	await h.command("maybe");
	assert.match(h.messages.at(-1)!, /Usage/);
	h.setIdle(false);
	await h.command("off");
	assert.match(h.messages.at(-1)!, /Wait for this turn/);
	assert.equal(h.runtime.cloudEnabled, true);
	assert.equal(h.entries.length, 0);
});

test("CA-CLOUD-3 Off removes Connection and memory instructions from model context", () => {
	const h = harness();
	h.runtime.connections = { xml: "private-connection-context", prefixes: ["aws"] };
	h.runtime.memory = { memoryIndex: "private-memory", userNotes: "private-notes" };
	h.runtime.setCloudEnabled(false);
	const prompt = buildPromptBlock(h.runtime);
	assert.doesNotMatch(prompt, /private-|ct_sandbox|ct_ask/);
	h.runtime.setCloudEnabled(true);
	assert.match(buildPromptBlock(h.runtime), /private-memory/);
});

test("CA-CLOUD-4 all five tools refuse execution while Off before calling the backend", async () => {
	const h = harness();
	for (const register of [registerSandboxRead, registerSandboxWrite, registerAsk, registerRunStatus, registerReadTaskOutput]) register(h.runtime);
	h.runtime.setCloudEnabled(false);
	for (const tool of h.tools.values()) {
		await assert.rejects(
			() => tool.execute("call", {}, undefined, undefined, h.ctx),
			/(?=.*Cloud is off)(?=.*CloudThinker Sandbox)(?=.*local tools keep running on your machine)(?=.*\/cloud on)/,
		);
	}
	assert.equal(h.tools.size, 5);
});
