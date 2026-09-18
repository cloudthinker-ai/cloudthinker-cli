import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { randomUUID } from "node:crypto";
import { DefaultResourceLoader, SessionManager, SettingsManager, createAgentSession, type AgentSession, type ExtensionAPI, type ExtensionContext } from "@earendil-works/pi-coding-agent";
import { runAgent, resumeAgent } from "@tintinweb/pi-subagents/dist/agent-runner.js";
import { registerAgents } from "@tintinweb/pi-subagents/dist/agent-types.js";
import cloudthinker from "@cloudthinker/pi/src/index.ts";
import { findLinkedSession } from "@cloudthinker/pi/src/session.ts";
import bundledSubagents, { createCloudChild, cloudDelegationTool } from "../src/subagents.ts";

test("CA-SUB-1/2/6/7/8/11: child lifecycle through the real upstream runtime", { timeout: 30_000 }, async () => {
	const root = mkdtempSync(join(tmpdir(), "ct-subagents-"));
	const old = { url: process.env.CLOUDTHINKER_URL, token: process.env.CLOUDTHINKER_TOKEN, dir: process.env.PI_CODING_AGENT_DIR };
	const sessions = new Map<string, { source?: string; entries: unknown[] }>();
	const calls: { model: string; conversation: string; tools: { name: string }[] }[] = [];
	const workspace = randomUUID();
	const server = createServer(async (request, response) => {
		const chunks = [];
		for await (const chunk of request) chunks.push(chunk);
		const body = chunks.length ? JSON.parse(Buffer.concat(chunks).toString()) : {};
		const path = new URL(request.url!, "http://localhost").pathname;
		let result: unknown = {};
		if (path.endsWith("/models")) result = { models: ["pro", "light"].map((id) => ({ id, name: id, reasoning: false, contextWindow: 200000, maxTokens: 4096, input: ["text"] })) };
		else if (path.endsWith("/whoami")) result = { workspace_id: workspace, workspace_name: "Fixture", user_email: "fixture@example.invalid" };
		else if (path.endsWith("/connections")) result = { xml: "", prefixes: [] };
		else if (path.endsWith("/sessions")) {
			const id = randomUUID();
			sessions.set(id, { source: body.source_conversation_id, entries: [] });
			result = { conversation_id: id, workspace_id: workspace, web_url: `http://localhost/${id}`, auto_mode: { enabled: false, can_edit: true } };
		} else if (path.endsWith("/entries")) {
			const session = sessions.get(path.split("/").at(-2)!)!;
			if (request.method === "PUT") session.entries.push(...body.entries);
			result = request.method === "PUT" ? { stored: body.entries.length, last_seq: session.entries.length } : { entries: [], last_seq: 0 };
		} else if (path.endsWith("/credits")) result = { credits_used: 0, tokens_consumed: 0 };
		else if (path.endsWith("/custom-skills/")) result = [];
		else if (path.endsWith("/executions")) result = { status: "completed", return_code: 0, stdout: "", stderr: "" };
		else if (path.endsWith("/messages")) {
			const conversation = String(request.headers["x-cloudthinker-conversation"] ?? "");
			calls.push({ model: body.model, conversation, tools: body.tools ?? [] });
			if (!sessions.has(conversation)) { response.writeHead(400); response.end("missing conversation"); return; }
			response.writeHead(200, { "Content-Type": "text/event-stream" });
			for (const event of [
				{ type: "message_start", message: { id: randomUUID(), type: "message", role: "assistant", model: body.model, content: [], stop_reason: null, usage: { input_tokens: 1, output_tokens: 0 } } },
				{ type: "content_block_start", index: 0, content_block: { type: "text", text: "" } },
				{ type: "content_block_delta", index: 0, delta: { type: "text_delta", text: "child verified" } },
				{ type: "content_block_stop", index: 0 },
				{ type: "message_delta", delta: { stop_reason: "end_turn", stop_sequence: null }, usage: { output_tokens: 2 } },
				{ type: "message_stop" },
			]) response.write(`event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`);
			response.end();
			return;
		}
		response.writeHead(200, { "Content-Type": "application/json" });
		response.end(JSON.stringify(result));
	});
	await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
	const address = server.address();
	assert.ok(address && typeof address !== "string");
	process.env.CLOUDTHINKER_URL = `http://127.0.0.1:${address.port}`;
	process.env.CLOUDTHINKER_TOKEN = randomUUID();
	process.env.PI_CODING_AGENT_DIR = root;
	let parent: AgentSession | undefined;
	const children: AgentSession[] = [];
	try {
		let ctx!: ExtensionContext;
		let pi!: ExtensionAPI;
		const settingsManager = SettingsManager.inMemory({ defaultProvider: "cloudthinker", defaultModel: "pro" });
		const loader = new DefaultResourceLoader({ cwd: root, agentDir: root, settingsManager, noExtensions: true, noSkills: true, noThemes: true, extensionFactories: [
			{ name: "cloudthinker", factory: (api) => cloudthinker(api) },
			{ name: "subagents", factory: bundledSubagents },
			{ name: "capture", factory: (api) => { pi = api; api.on("session_start", (_event, context) => { ctx = context; }); } },
		] });
		await loader.reload();
		assert.deepEqual(loader.getExtensions().errors, []);
		const tools = loader.getExtensions().extensions.flatMap((extension) => [...extension.tools.values()].map((tool) => tool.definition));
		for (const name of ["Agent", "SubagentWorkflow"]) {
			const tool = tools.find((definition) => definition.name === name)!;
			assert.ok(tool);
			assert.equal(cloudDelegationTool(tool), tool);
			assert.doesNotMatch(JSON.stringify(tool.parameters), /"thinking"/);
			assert.doesNotMatch(tool.description, /haiku|sonnet|opts\.effort|Use thinking/);
		}
		parent = (await createAgentSession({ cwd: root, agentDir: root, settingsManager, resourceLoader: loader, sessionManager: SessionManager.create(root) })).session;
		await parent.bindExtensions({});
		assert.ok(parent.getAllTools().some((tool) => tool.name === "Agent"));
		assert.ok(pi.getActiveTools().includes("SubagentWorkflow"));
		await parent.setModel(ctx.modelRegistry.find("cloudthinker", "pro")!);
		ctx = { ...ctx, model: parent.model };
		assert.equal(ctx.model?.provider, "cloudthinker");
		const parentId = findLinkedSession(parent.sessionManager.getEntries())!.conversation_id;
		const results = await Promise.all(["general-purpose", "Explore"].map((type) => runAgent(ctx, type, "Reply child verified", { pi, onSessionCreated: (child) => children.push(child) })));
		for (const result of results) { assert.equal(result.failure, undefined); assert.equal(result.responseText, "child verified"); }
		assert.equal(calls.length, 2);
		assert.equal(new Set(calls.map((call) => call.conversation)).size, 2);
		for (const call of calls) {
			assert.equal(call.model, "pro");
			assert.equal(sessions.get(call.conversation)?.source, parentId);
		}
		for (const child of children) assert.match(readFileSync(child.sessionManager.getSessionFile()!, "utf8"), /child verified/);
		registerAgents(new Map([["invalid", { name: "invalid", description: "Invalid mode", model: "anthropic/claude-opus", systemPrompt: "", promptMode: "replace", extensions: false, skills: false }]]));
		await assert.rejects(runAgent(ctx, "invalid", "Never send", { pi }), /CloudThinker agent mode/);
		assert.equal(calls.length, 2);
		const resumed = await resumeAgent(children[0]!, "Reply child verified again");
		assert.equal(resumed.text, "child verified");
		assert.equal(calls.at(-1)!.conversation, findLinkedSession(children[0]!.sessionManager.getEntries())!.conversation_id);
		const isolated = await runAgent(ctx, "general-purpose", "Reply child verified", {
			pi, isolated: true, nested: true, workflow: true,
			model: ctx.modelRegistry.find("cloudthinker", "light")!,
			onSessionCreated: (child) => children.push(child),
		});
		assert.equal(isolated.failure, undefined);
		assert.equal(calls.at(-1)!.model, "light");
		const reopened = await runAgent(ctx, "general-purpose", "Reply child verified", {
			pi, resumeSessionFile: children[0]!.sessionManager.getSessionFile(),
			onSessionCreated: (child) => children.push(child),
		});
		assert.equal(reopened.failure, undefined);
		assert.equal(calls.at(-1)!.conversation, findLinkedSession(children[0]!.sessionManager.getEntries())!.conversation_id);
		const reopenedLight = await runAgent(ctx, "general-purpose", "Keep the saved Light mode", {
			pi, resumeSessionFile: children[2]!.sessionManager.getSessionFile(),
			onSessionCreated: (child) => children.push(child),
		});
		assert.equal(reopenedLight.failure, undefined);
		assert.equal(calls.at(-1)!.model, "light");
		pi.appendEntry("cloudthinker.cloud", { enabled: true });
		const cloudOn = await runAgent(ctx, "general-purpose", "Cloud On regular child", { pi, onSessionCreated: (child) => children.push(child) });
		assert.equal(cloudOn.failure, undefined);
		assert.ok(calls.at(-1)!.tools.some((tool) => tool.name === "ct_ask"));
		const clone = await createCloudChild({ cwd: root, model: ctx.model }, ctx);
		children.push(clone.session);
		await clone.session.prompt("Cloud On child without a supplied loader");
		assert.ok(calls.at(-1)!.tools.some((tool) => tool.name === "ct_ask"));
		registerAgents(new Map([["excluded", { name: "excluded", description: "No cloud extension", systemPrompt: "", promptMode: "replace", extensions: ["unrelated"], skills: false }]]));
		const excluded = await runAgent(ctx, "excluded", "Reply child verified", { pi, onSessionCreated: (child) => children.push(child) });
		assert.equal(excluded.failure, undefined);
		assert.ok(calls.at(-1)!.tools.every((tool) => !["ct_ask", "ct_sandbox_read", "ct_sandbox_write", "ct_run_status", "read_task_output"].includes(tool.name)));
		const agentTool = tools.find((tool) => tool.name === "Agent")!;
		const direct = await agentTool.execute("headless-agent", {
			subagent_type: "general-purpose", description: "Headless child", prompt: "Reply child verified", run_in_background: true,
		}, undefined, undefined, { ...ctx, mode: "json" });
		assert.match(JSON.stringify(direct.content), /child verified/);
		const workflowTool = tools.find((tool) => tool.name === "SubagentWorkflow")!;
		const workflow = await workflowTool.execute("headless-workflow", {
			script: 'export const meta = {name:"headless",description:"Wait for children"}; return await agent("Reply child verified", {model:"cloudthinker/light"});',
		}, undefined, undefined, { ...ctx, mode: "json" });
		assert.match(JSON.stringify(workflow.content), /child verified/);
		assert.doesNotMatch(JSON.stringify(workflow.content), /started in the background/);
		clone.session.sessionManager.appendModelChange("anthropic", "claude-opus");
		await assert.rejects(createCloudChild({ cwd: root, model: ctx.model, sessionManager: clone.session.sessionManager }, ctx), /CloudThinker agent mode/);
	} finally {
		for (const child of children) {
			await child.extensionRunner.emit({ type: "session_shutdown", reason: "quit" });
			child.dispose();
		}
		await parent?.extensionRunner.emit({ type: "session_shutdown", reason: "quit" });
		parent?.dispose();
		await new Promise<void>((resolve) => server.close(() => resolve()));
		for (const [key, value] of Object.entries({ CLOUDTHINKER_URL: old.url, CLOUDTHINKER_TOKEN: old.token, PI_CODING_AGENT_DIR: old.dir })) {
			if (value === undefined) delete process.env[key]; else process.env[key] = value;
		}
		rmSync(root, { recursive: true, force: true });
	}
});
