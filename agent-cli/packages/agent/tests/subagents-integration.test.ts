import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
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
	let holdModelResponses = false;
	let heldModelRequestCount = 0;
	const heldModelReleases: (() => void)[] = [];
	const heldModelWaiters: { count: number; resolve: () => void }[] = [];
	let workflowNotificationCount = 0;
	const workflowNotificationWaiters: { count: number; resolve: () => void }[] = [];
	const waitForHeldModelRequests = (count: number) => {
		if (heldModelRequestCount >= count) return Promise.resolve();
		return new Promise<void>((resolve) => heldModelWaiters.push({ count, resolve }));
	};
	const waitForWorkflowNotifications = (count: number) => {
		if (workflowNotificationCount >= count) return Promise.resolve();
		return new Promise<void>((resolve) => workflowNotificationWaiters.push({ count, resolve }));
	};
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
			if (holdModelResponses) {
				heldModelRequestCount++;
				for (const waiter of heldModelWaiters.splice(0)) {
					if (heldModelRequestCount >= waiter.count) waiter.resolve();
					else heldModelWaiters.push(waiter);
				}
				await new Promise<void>((resolve) => heldModelReleases.push(resolve));
			}
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
			{ name: "capture", factory: (api) => {
				pi = api;
				api.on("session_start", (_event, context) => { ctx = context; });
				api.on("message_end", (event) => {
					if (event.message.role !== "custom" || event.message.customType !== "subagent-notification") return;
					workflowNotificationCount++;
					for (const waiter of workflowNotificationWaiters.splice(0)) {
						if (workflowNotificationCount >= waiter.count) waiter.resolve();
						else workflowNotificationWaiters.push(waiter);
					}
				});
			} },
		] });
		await loader.reload();
		assert.deepEqual(loader.getExtensions().errors, []);
		const tools = loader.getExtensions().extensions.flatMap((extension) => [...extension.tools.values()].map((tool) => tool.definition));
		for (const name of ["Agent", "ct_workflow"]) {
			const tool = tools.find((definition) => definition.name === name)!;
			assert.ok(tool);
			assert.equal(cloudDelegationTool(tool), tool);
			assert.doesNotMatch(JSON.stringify(tool.parameters), /"thinking"/);
			assert.doesNotMatch(tool.description, /haiku|sonnet|opts\.effort|Use thinking/);
			if (name === "ct_workflow") {
				assert.equal(tool.label, "ct_workflow");
				assert.match(tool.description, /plain-language task/);
				assert.match(tool.description, /Use \/agents → Workflows to watch live progress/);
				assert.doesNotMatch(tool.description, /SubagentWorkflow/);
				assert.doesNotMatch(tool.description, /Cyber|pentest/);
				const schema = tool.parameters as { properties?: Record<string, { type?: string; description?: string; additionalProperties?: boolean }>; required?: string[] };
				assert.equal(schema.properties?.args?.type, "object");
				assert.equal(schema.properties?.args?.additionalProperties, true);
				assert.match(schema.properties?.args?.description ?? "", /not JSON-encoded strings/);
				assert.ok(!schema.required?.includes("args"));
			}
		}
		parent = (await createAgentSession({ cwd: root, agentDir: root, settingsManager, resourceLoader: loader, sessionManager: SessionManager.create(root) })).session;
		await parent.bindExtensions({});
		assert.ok(parent.getAllTools().some((tool) => tool.name === "Agent"));
		assert.ok(pi.getActiveTools().includes("ct_workflow"));
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
		const workflowTool = tools.find((tool) => tool.name === "ct_workflow")!;
		const callsBeforeInvalidWorkflow = calls.length;
		await assert.rejects(workflowTool.execute("headless-invalid-workflow", {
			args: JSON.stringify({ run: "fixture" }),
			script: 'export const meta = {name:"invalid args",description:"Must reject before dispatch"}; return await agent("This agent must not be dispatched");',
		}, undefined, undefined, { ...ctx, mode: "json" }), /ct_workflow args must be an object.*do not pass a JSON-encoded string/);
		assert.equal(calls.length, callsBeforeInvalidWorkflow);
		for (const [id, args] of [["string", "literal"], ["number", 5], ["array", ["fixture"]], ["null", null]] as const) {
			await assert.rejects(workflowTool.execute(`headless-invalid-${id}-workflow`, {
				args,
				script: `export const meta = {name:"invalid ${id} args",description:"Must reject non-object args"}; return "unreachable";`,
			}, undefined, undefined, { ...ctx, mode: "json" }), /ct_workflow args must be an object/);
		}
		assert.equal(calls.length, callsBeforeInvalidWorkflow);
		const argsWorkflow = await workflowTool.execute("headless-object-args-workflow", {
			args: { run: "object-args-round-trip" },
			script: 'export const meta = {name:"object args",description:"Preserve object args"}; return args.run;',
		}, undefined, undefined, { ...ctx, mode: "json" });
		assert.match(JSON.stringify(argsWorkflow.content), /object-args-round-trip/);
		const workflow = await workflowTool.execute("headless-workflow", {
			script: 'export const meta = {name:"headless",description:"Wait for children"}; return await agent("Reply child verified", {model:"cloudthinker/light"});',
		}, undefined, undefined, { ...ctx, mode: "json" });
		assert.match(JSON.stringify(workflow.content), /child verified/);
		assert.doesNotMatch(JSON.stringify(workflow.content), /started in the background/);
		await assert.rejects(workflowTool.execute("headless-failed-workflow", {
			script: 'export const meta = {name:"headless failure",description:"Fail while running"}; throw new Error("workflow runtime failed");',
		}, undefined, undefined, { ...ctx, mode: "json" }), /workflow runtime failed/);
		const originalScript = 'export const meta = {name:"active identity",description:"Active identity fixture"}; return await agent("Hold this workflow until released");';
		const changedScript = 'export const meta = {name:"active identity",description:"Active identity fixture"}; return await agent("Changed source starts distinct work");';
		const workflowContext = { ...ctx, mode: "tui" as const };
		const childCallsBeforeActiveStarts = calls.filter((call) => call.conversation !== parentId).length;
		const notificationsBeforeActiveStarts = workflowNotificationCount;
		holdModelResponses = true;
		const firstHeldRequest = waitForHeldModelRequests(1);
		const firstActiveWorkflow = await workflowTool.execute("active-workflow-first", {
			script: originalScript,
			args: { run: "same", options: { alpha: 1, beta: 2 } },
		}, undefined, undefined, workflowContext);
		const firstActiveId = (firstActiveWorkflow.details as { taskId: string }).taskId;
		await firstHeldRequest;
		const firstActiveScriptPath = String(firstActiveWorkflow.content[0]?.type === "text" && firstActiveWorkflow.content[0].text.match(/^Script: (.+)$/m)?.[1]);
		assert.notEqual(firstActiveScriptPath, "undefined");
		const duplicateActiveWorkflow = await workflowTool.execute("active-workflow-duplicate", {
			scriptPath: firstActiveScriptPath,
			args: { options: { beta: 2, alpha: 1 }, run: "same" },
		}, undefined, undefined, workflowContext);
		assert.equal((duplicateActiveWorkflow.details as { taskId?: string } | undefined)?.taskId, firstActiveId);
		assert.equal(calls.filter((call) => call.conversation !== parentId).length, childCallsBeforeActiveStarts + 1);
		const activeResume = await workflowTool.execute("active-workflow-resume", {
			resumeFromRunId: firstActiveId,
		}, undefined, undefined, workflowContext);
		assert.equal((activeResume.details as { taskId?: string } | undefined)?.taskId, firstActiveId);
		assert.equal(calls.filter((call) => call.conversation !== parentId).length, childCallsBeforeActiveStarts + 1);
		let headlessDuplicateFinished = false;
		const headlessDuplicatePromise = workflowTool.execute("active-workflow-headless-duplicate", {
			scriptPath: firstActiveScriptPath,
			args: { options: { alpha: 1, beta: 2 }, run: "same" },
		}, undefined, undefined, { ...ctx, mode: "json" }).then((result) => {
			headlessDuplicateFinished = true;
			return result;
		});
		await Promise.resolve();
		assert.equal(headlessDuplicateFinished, false);
		writeFileSync(firstActiveScriptPath, changedScript);
		const secondHeldRequest = waitForHeldModelRequests(2);
		const changedActiveWorkflow = await workflowTool.execute("active-workflow-changed-source", {
			scriptPath: firstActiveScriptPath,
			args: { run: "same" },
		}, undefined, undefined, workflowContext);
		const changedActiveId = (changedActiveWorkflow.details as { taskId: string }).taskId;
		assert.notEqual(changedActiveId, firstActiveId);
		await secondHeldRequest;
		const thirdHeldRequest = waitForHeldModelRequests(3);
		const changedArgsWorkflow = await workflowTool.execute("active-workflow-changed-args", {
			scriptPath: firstActiveScriptPath,
			args: { run: "different" },
		}, undefined, undefined, workflowContext);
		const changedArgsId = (changedArgsWorkflow.details as { taskId: string }).taskId;
		assert.notEqual(changedArgsId, changedActiveId);
		await thirdHeldRequest;
		holdModelResponses = false;
		for (const release of heldModelReleases.splice(0)) release();
		const headlessDuplicate = await headlessDuplicatePromise;
		assert.match(JSON.stringify(headlessDuplicate.content), /child verified/);
		await waitForWorkflowNotifications(notificationsBeforeActiveStarts + 3);
		writeFileSync(firstActiveScriptPath, originalScript);
		const completedRerunRequest = waitForWorkflowNotifications(notificationsBeforeActiveStarts + 4);
		const completedRerun = await workflowTool.execute("completed-workflow-rerun", {
			scriptPath: firstActiveScriptPath,
			args: { options: { alpha: 1, beta: 2 }, run: "same" },
		}, undefined, undefined, workflowContext);
		assert.notEqual((completedRerun.details as { taskId: string }).taskId, firstActiveId);
		await completedRerunRequest;
		assert.equal(calls.filter((call) => call.conversation !== parentId).length, childCallsBeforeActiveStarts + 4);
		clone.session.sessionManager.appendModelChange("anthropic", "claude-opus");
		await assert.rejects(createCloudChild({ cwd: root, model: ctx.model, sessionManager: clone.session.sessionManager }, ctx), /CloudThinker agent mode/);
	} finally {
		holdModelResponses = false;
		for (const release of heldModelReleases.splice(0)) release();
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
