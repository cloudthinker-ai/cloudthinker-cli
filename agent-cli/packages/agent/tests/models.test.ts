import assert from "node:assert/strict";
import test from "node:test";

import { parseArgs, resolveModelScopeWithDiagnostics } from "@earendil-works/pi-coding-agent";
import type { ModelRuntime } from "@earendil-works/pi-coding-agent";

import { MODEL_SCOPE, modelScopeArgs } from "../src/models.ts";
import { bundledThemePaths, themeArgs } from "../src/theme.ts";

const AVAILABLE = [
	{ provider: "cloudthinker", id: "pro" },
	{ provider: "cloudthinker", id: "ultra" },
	{ provider: "amazon-bedrock", id: "us.anthropic.claude-opus-4-6-v1" },
	{ provider: "anthropic", id: "claude-opus-5" },
];

const runtime = { getAvailable: async () => AVAILABLE } as unknown as ModelRuntime;

function piArgv(argv: string[]): string[] {
	const themes = themeArgs(bundledThemePaths("/opt/cloudthinker-agent/theme", () => true), argv, undefined);
	return [...themes, ...modelScopeArgs(argv), ...argv];
}

test("the model picker is scoped to the CloudThinker modes", () => {
	assert.equal(MODEL_SCOPE, "cloudthinker/*");
	assert.deepEqual(modelScopeArgs([]), ["--models", MODEL_SCOPE]);
	assert.deepEqual(modelScopeArgs(["--workspace", "ops"]), ["--models", MODEL_SCOPE]);
});

test("the caller's own model scope is never overridden", () => {
	assert.deepEqual(modelScopeArgs(["--models", "cloudthinker/ultra"]), []);
});

test("pi parses the injected scope and resolves it to the modes alone", async () => {
	const parsed = parseArgs(piArgv(["--workspace", "ops"]));
	assert.deepEqual(parsed.models, [MODEL_SCOPE]);
	assert.deepEqual(parsed.messages, []);

	const { scopedModels, diagnostics } = await resolveModelScopeWithDiagnostics(parsed.models ?? [], runtime);
	assert.deepEqual(diagnostics, []);
	assert.deepEqual(
		scopedModels.map((scoped) => `${scoped.model.provider}/${scoped.model.id}`),
		["cloudthinker/pro", "cloudthinker/ultra"],
	);
});

test("pi takes the caller's --models as the only scope", async () => {
	const parsed = parseArgs(piArgv(["--models", "cloudthinker/ultra"]));
	assert.deepEqual(parsed.models, ["cloudthinker/ultra"]);

	const { scopedModels } = await resolveModelScopeWithDiagnostics(parsed.models ?? [], runtime);
	assert.deepEqual(
		scopedModels.map((scoped) => scoped.model.id),
		["ultra"],
	);
});

test("pi reads no scope from the --models=value form, so the default still applies", () => {
	const parsed = parseArgs(piArgv(["--models=cloudthinker/ultra"]));
	assert.deepEqual(parsed.models, [MODEL_SCOPE]);
	assert.equal(parsed.unknownFlags.get("models"), "cloudthinker/ultra");
});
