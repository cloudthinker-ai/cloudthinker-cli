import assert from "node:assert/strict";
import test from "node:test";

import { parseArgs, resolveModelScopeWithDiagnostics } from "@earendil-works/pi-coding-agent";
import type { ModelRuntime } from "@earendil-works/pi-coding-agent";

import { ModelRuntime as RuntimeClass } from "../node_modules/@earendil-works/pi-coding-agent/dist/core/model-runtime.js";
import { findInitialModel } from "../node_modules/@earendil-works/pi-coding-agent/dist/core/model-resolver.js";
import { formatNoApiKeyFoundMessage, formatNoModelSelectedMessage } from "../node_modules/@earendil-works/pi-coding-agent/dist/core/auth-guidance.js";
import { MODEL_SCOPE, NO_CLOUD_MODEL, applyCloudOnlyModels, cloudModelError, modelScopeArgs } from "../src/models.ts";
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

test("with the agent modes missing, pi starts with no model instead of a vendor model it has credentials for", async () => {
	applyCloudOnlyModels();
	const offline = [AVAILABLE[2]!, AVAILABLE[3]!];
	const runtimeWith = (available: readonly object[]) => Object.assign(Object.create(RuntimeClass.prototype), {
		snapshot: { available, configuredProviders: new Set(["cloudthinker", "amazon-bedrock", "anthropic"]) },
		models: { getModel: (provider: string, id: string) => AVAILABLE.find((model) => model.provider === provider && model.id === id) },
	});
	const start = (available: readonly object[]) => findInitialModel({
		scopedModels: [],
		isContinuing: false,
		defaultProvider: "anthropic",
		defaultModelId: "claude-opus-5",
		modelRuntime: runtimeWith(available),
	} as never);
	assert.equal((await start(offline)).model, undefined);
	assert.deepEqual((await start(AVAILABLE)).model, AVAILABLE[0]);
	assert.equal(runtimeWith(AVAILABLE).hasConfiguredAuth("amazon-bedrock"), false);
});

test("a prompt with no agent mode loaded points at /cloud retry instead of vendor login", () => {
	const guided = (message: string, model?: { provider: string }) => (cloudModelError(new Error(message), model) as Error).message;
	assert.equal(guided(formatNoModelSelectedMessage()), NO_CLOUD_MODEL);
	assert.equal(guided(formatNoApiKeyFoundMessage("unknown"), { provider: "unknown" }), NO_CLOUD_MODEL);
	assert.equal(guided(formatNoApiKeyFoundMessage("anthropic"), { provider: "anthropic" }), NO_CLOUD_MODEL);
	assert.equal(guided(formatNoApiKeyFoundMessage("cloudthinker"), { provider: "cloudthinker" }), formatNoApiKeyFoundMessage("cloudthinker"));
	assert.equal(guided("Connection refused", { provider: "anthropic" }), "Connection refused");
});
