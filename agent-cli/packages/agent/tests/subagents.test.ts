import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import type { Extension, ExtensionContext, LoadExtensionsResult, ToolDefinition } from "@earendil-works/pi-coding-agent";

import { CLOUD_ENTRY_TYPE } from "@cloudthinker/pi/src/runtime.ts";
import { childLoaderOptions, cloudDelegationTool, cloudEnabled, resolveCloudMode } from "../src/subagents.ts";

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

test("CA-SUB-8: a child of a setting-off parent gets no cloud tools, and an entry still wins", () => {
	const root = mkdtempSync(join(tmpdir(), "ct-cloud-default-"));
	const previous = process.env.PI_CODING_AGENT_DIR;
	process.env.PI_CODING_AGENT_DIR = join(root, "agent");
	mkdirSync(join(root, ".pi"), { recursive: true });
	writeFileSync(join(root, ".pi", "settings.json"), JSON.stringify({ cloudDefault: false }));
	try {
		const entries: unknown[] = [];
		const ctx = { cwd: root, sessionManager: { getEntries: () => entries }, isProjectTrusted: () => true } as unknown as ExtensionContext;
		const untrusted = { cwd: root, sessionManager: { getEntries: () => entries }, isProjectTrusted: () => false } as unknown as ExtensionContext;
		const load = () => childLoaderOptions({ cwd: root, agentDir: root, noExtensions: true }, ctx);
		const base = (cloud: Extension): LoadExtensionsResult => ({ extensions: [cloud], errors: [], runtime: {} } as unknown as LoadExtensionsResult);

		assert.equal(cloudEnabled(ctx), false);
		assert.equal(cloudEnabled(untrusted), true);
		const off = { path: "<inline:cloudthinker>", tools: new Map([["ct_ask", {}]]) } as unknown as Extension;
		load().extensionsOverride?.(base(off));
		assert.equal(off.tools.size, 0);

		entries.push({ type: "custom", customType: CLOUD_ENTRY_TYPE, data: { enabled: true } });
		assert.equal(cloudEnabled(ctx), true);
		assert.equal(cloudEnabled(untrusted), true);
		const on = { path: "<inline:cloudthinker>", tools: new Map([["ct_ask", {}]]) } as unknown as Extension;
		load().extensionsOverride?.(base(on));
		assert.equal(on.tools.size, 1);
	} finally {
		if (previous === undefined) delete process.env.PI_CODING_AGENT_DIR;
		else process.env.PI_CODING_AGENT_DIR = previous;
		rmSync(root, { recursive: true, force: true });
	}
});
