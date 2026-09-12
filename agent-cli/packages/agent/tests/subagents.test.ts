import assert from "node:assert/strict";
import test from "node:test";
import type { ToolDefinition } from "@earendil-works/pi-coding-agent";

import { resolveCloudMode, cloudDelegationTool } from "../src/subagents.ts";

const modes = [
	{ provider: "cloudthinker", id: "light", name: "Light" },
	{ provider: "cloudthinker", id: "pro", name: "Pro" },
	{ provider: "anthropic", id: "claude-opus", name: "Opus" },
];
const registry = {
	getAll: () => modes,
	getAvailable: () => modes,
	find: (provider: string, id: string) => modes.find((mode) => mode.provider === provider && mode.id === id),
};

test("CA-SUB-3: child mode resolves only exact advertised CloudThinker modes", () => {
	assert.equal(resolveCloudMode("light", registry), modes[0]);
	assert.equal(resolveCloudMode("cloudthinker/pro", registry), modes[1]);
});

test("CA-SUB-4: vendor credentials and fuzzy names cannot escape CloudThinker modes", () => {
	for (const name of ["anthropic/claude-opus", "opus", "cl", "cloudthinker/removed", ""]) {
		assert.match(String(resolveCloudMode(name, registry)), /CloudThinker agent mode/);
	}
});

test("CA-SUB-5: empty catalog does not select a vendor model", () => {
	assert.match(String(resolveCloudMode("pro", { ...registry, getAll: () => [modes[2]!] })), /CloudThinker agent mode/);
});

test("CA-SUB-DESCRIPTION: upstream description drift fails closed", () => {
	assert.throws(() => cloudDelegationTool({ name: "Agent", description: "Unrecognized upstream schema", parameters: { type: "object", properties: {} } } as unknown as ToolDefinition), /Unsupported upstream Agent description/);
});
