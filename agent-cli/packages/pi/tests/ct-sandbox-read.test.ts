import assert from "node:assert/strict";
import test from "node:test";

import type { ExtensionAPI, ToolDefinition } from "@earendil-works/pi-coding-agent";

import type { CloudThinkerClient } from "../src/client.ts";
import { MEMORY_DIR } from "../src/memory.ts";
import { CloudThinkerRuntime } from "../src/runtime.ts";
import { registerSandboxRead } from "../src/tools/ct-sandbox-read.ts";
import { CT_SANDBOX_READ } from "../src/tools/names.ts";
import { hostVersionsFrom } from "../src/versions.ts";

function readTool(): ToolDefinition {
	const tools = new Map<string, ToolDefinition>();
	const runtime = new CloudThinkerRuntime(
		{
			registerTool: (tool: ToolDefinition) => tools.set(tool.name, tool),
			appendEntry: () => {},
		} as unknown as ExtensionAPI,
		{} as CloudThinkerClient,
		hostVersionsFrom({ version: "0.4.0", piVersion: "0.85.1" }, "0.4.0"),
	);
	registerSandboxRead(runtime);
	const tool = tools.get(CT_SANDBOX_READ);
	assert.ok(tool);
	return tool;
}

test("the description names the Sandbox as the workspace's own durable machine", () => {
	const { description, promptSnippet } = readTool();
	assert.ok(description.includes("the workspace's own machine in the cloud"));
	assert.ok(description.includes("durable machine the whole workspace shares, not a scratch shell"));
	assert.ok(promptSnippet?.includes("CloudThinker Sandbox"));
});

test("the description says a Sandbox-only read needs no Connection", () => {
	const { description } = readTool();
	assert.ok(description.includes(`memory tree at ${MEMORY_DIR}`));
	assert.ok(description.includes("Pass an empty connection_list"));
});
