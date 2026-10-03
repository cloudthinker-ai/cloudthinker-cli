import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync, chmodSync } from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, join } from "node:path";
import test from "node:test";

import { ModelRuntime, type ExtensionAPI } from "@earendil-works/pi-coding-agent";

import { CloudThinkerClient, TokenSource } from "../src/client.ts";
import {
	DEFAULT_MODE,
	NO_MODES_REASON,
	NO_PRICE,
	PROVIDER_ID,
	apiKeySpec,
	modelsUnavailableMessage,
	pinProviderWorkspace,
	registerProvider,
} from "../src/provider.ts";
import { CloudThinkerRuntime } from "../src/runtime.ts";
import { startFakeServer } from "./helpers.ts";

interface Registration {
	name: string;
	config: {
		baseUrl?: string;
		apiKey?: string;
		models?: { id: string; cost?: Record<string, number> }[];
	};
}

function runtimeFor(origin: string): {
	runtime: CloudThinkerRuntime;
	registrations: Registration[];
} {
	const registrations: Registration[] = [];
	const pi = {
		registerProvider: (name: string, config: Registration["config"]) => {
			registrations.push({ name, config });
		},
	} as unknown as ExtensionAPI;
	const client = new CloudThinkerClient({
		baseUrl: origin,
		tokens: new TokenSource({ CLOUDTHINKER_TOKEN: "t" }),
	});
	return { runtime: new CloudThinkerRuntime(pi, client), registrations };
}

const MODES = [
	{ id: "ultra", name: "Ultra" },
	{ id: "light", name: "Light" },
	{ id: "pro", name: "Pro" },
].map((mode) => ({
	...mode,
	reasoning: true,
	contextWindow: 200_000,
	maxTokens: 32_000,
	input: ["text", "image"],
}));

test("no mode carries a price, because pi would render it as dollars", async () => {
	const server = await startFakeServer((request) =>
		request.path === "/api/v1/agent-cli/models" ? { body: { models: MODES } } : undefined,
	);
	try {
		const { runtime, registrations } = runtimeFor(server.origin);
		await registerProvider(runtime);

		for (const model of registrations[0]?.config.models ?? []) {
			assert.deepEqual(model.cost, { ...NO_PRICE });
		}
	} finally {
		await server.close();
	}
});

test("the gateway's modes are registered with pro first", async () => {
	const server = await startFakeServer((request) =>
		request.path === "/api/v1/agent-cli/models" ? { body: { models: MODES } } : undefined,
	);
	try {
		const { runtime, registrations } = runtimeFor(server.origin);
		const failure = await registerProvider(runtime);

		assert.equal(failure, undefined);
		assert.equal(registrations.length, 1);
		const [only] = registrations;
		assert.equal(only?.name, PROVIDER_ID);
		assert.equal(only?.config.baseUrl, `${server.origin}/api/v1/agent-cli/llm`);
		assert.deepEqual(
			only?.config.models?.map((model) => model.id),
			[DEFAULT_MODE, "light", "ultra"],
		);
	} finally {
		await server.close();
	}
});

test("a failed model listing registers no provider and reports the reason", async () => {
	const server = await startFakeServer((request) =>
		request.path === "/api/v1/agent-cli/models"
			? { status: 503, body: { detail: "gateway is down" } }
			: undefined,
	);
	try {
		const { runtime, registrations } = runtimeFor(server.origin);
		const failure = await registerProvider(runtime);

		assert.deepEqual(registrations, []);
		assert.equal(failure, "gateway is down");
		assert.match(modelsUnavailableMessage(failure), /gateway is down/);
		assert.match(modelsUnavailableMessage(failure), /\/cloud retry loads them/);
	} finally {
		await server.close();
	}
});

test("an empty model listing registers no provider, so pi never falls back to a vendor model", async () => {
	const server = await startFakeServer((request) =>
		request.path === "/api/v1/agent-cli/models" ? { body: { models: [] } } : undefined,
	);
	try {
		const { runtime, registrations } = runtimeFor(server.origin);
		const failure = await registerProvider(runtime);

		assert.deepEqual(registrations, []);
		assert.equal(failure, NO_MODES_REASON);
		assert.match(modelsUnavailableMessage(failure), /no agent mode/);
	} finally {
		await server.close();
	}
});

const WORKSPACE_ENV = "33333333-3333-4333-8333-333333333333";
const WORKSPACE_SESSION = "44444444-4444-4444-8444-444444444444";

test("the token spec is pinned by the environment until a session names the workspace", () => {
	assert.equal(apiKeySpec(undefined, {}), "!cloudthinker auth token");
	assert.equal(
		apiKeySpec(undefined, { CLOUDTHINKER_WORKSPACE: WORKSPACE_ENV }),
		`!cloudthinker auth token --workspace ${WORKSPACE_ENV}`,
	);
	assert.equal(
		apiKeySpec(WORKSPACE_SESSION, { CLOUDTHINKER_WORKSPACE: WORKSPACE_ENV }),
		`!cloudthinker auth token --workspace ${WORKSPACE_SESSION}`,
	);
	assert.equal(
		apiKeySpec(WORKSPACE_SESSION, { CLOUDTHINKER_TOKEN: "secret" }),
		"$CLOUDTHINKER_TOKEN",
	);
	assert.equal(
		apiKeySpec(undefined, { CLOUDTHINKER_TOKEN: "secret", CLOUDTHINKER_WORKSPACE: WORKSPACE_ENV }),
		"$CLOUDTHINKER_TOKEN",
	);
});

test("the token spec runs the wrapper's own binary", () => {
	assert.equal(
		apiKeySpec(WORKSPACE_SESSION, { CLOUDTHINKER_BIN: "/home/u/.cargo/bin/cloudthinker" }),
		`!/home/u/.cargo/bin/cloudthinker auth token --workspace ${WORKSPACE_SESSION}`,
	);
	assert.equal(apiKeySpec(undefined, { CLOUDTHINKER_BIN: "/x y/cloudthinker" }), "!cloudthinker auth token");
});

test("a workspace that is not a UUID never reaches the token shell command", () => {
	assert.equal(apiKeySpec("ws; touch /tmp/pwned", {}), "!cloudthinker auth token");
	assert.equal(apiKeySpec(undefined, { CLOUDTHINKER_WORKSPACE: "$(id)" }), "!cloudthinker auth token");
	assert.equal(apiKeySpec(undefined, { CLOUDTHINKER_WORKSPACE: `${WORKSPACE_ENV} --x` }), "!cloudthinker auth token");
});

test("the session's workspace re-registers the provider without listing the modes again", async () => {
	const token = process.env.CLOUDTHINKER_TOKEN;
	const workspace = process.env.CLOUDTHINKER_WORKSPACE;
	const binary = process.env.CLOUDTHINKER_BIN;
	delete process.env.CLOUDTHINKER_TOKEN;
	delete process.env.CLOUDTHINKER_WORKSPACE;
	delete process.env.CLOUDTHINKER_BIN;
	let listings = 0;
	const server = await startFakeServer((request) => {
		if (request.path !== "/api/v1/agent-cli/models") return undefined;
		listings += 1;
		return { body: { models: MODES } };
	});
	try {
		const { runtime, registrations } = runtimeFor(server.origin);
		await registerProvider(runtime);
		pinProviderWorkspace(runtime, WORKSPACE_SESSION);

		assert.equal(listings, 1);
		assert.equal(registrations.length, 2);
		assert.equal(registrations[0]?.config.apiKey, "!cloudthinker auth token");
		assert.equal(
			registrations[1]?.config.apiKey,
			`!cloudthinker auth token --workspace ${WORKSPACE_SESSION}`,
		);
		assert.deepEqual(
			registrations[1]?.config.models?.map((model) => model.id),
			[DEFAULT_MODE, "light", "ultra"],
		);
	} finally {
		if (token !== undefined) process.env.CLOUDTHINKER_TOKEN = token;
		if (workspace !== undefined) process.env.CLOUDTHINKER_WORKSPACE = workspace;
		if (binary !== undefined) process.env.CLOUDTHINKER_BIN = binary;
		await server.close();
	}
});

test("a failed model listing leaves the workspace pin nothing to register", async () => {
	const server = await startFakeServer((request) =>
		request.path === "/api/v1/agent-cli/models" ? { status: 503, body: {} } : undefined,
	);
	try {
		const { runtime, registrations } = runtimeFor(server.origin);
		await registerProvider(runtime);
		pinProviderWorkspace(runtime, "ws-session");

		assert.deepEqual(registrations, []);
	} finally {
		await server.close();
	}
});

test("a failed stored auth command is retried and can recover", async () => {
	const bin = mkdtempSync(join(tmpdir(), "pi-provider-auth-"));
	const previousPath = process.env.PATH;
	const previousAnthropicKey = process.env.ANTHROPIC_API_KEY;
	process.env.PATH = `${bin}${delimiter}${previousPath ?? ""}`;
	delete process.env.ANTHROPIC_API_KEY;
	const cloudCommand = "!ct-cloudthinker-auth";
	const authPath = join(bin, "auth.json");
	writeFileSync(authPath, JSON.stringify({ anthropic: { type: "api_key", key: cloudCommand } }));
	const models = await ModelRuntime.create({ authPath, modelsPath: null, refreshOnCreate: false });
	try {
		assert.equal(await models.getAuth("anthropic"), undefined);
		const commandPath = join(bin, cloudCommand.slice(1));
		writeFileSync(commandPath, "#!/bin/sh\nprintf 'resolved-token\\n'\n");
		chmodSync(commandPath, 0o700);
		assert.equal((await models.getAuth("anthropic"))?.auth.apiKey, "resolved-token");
	} finally {
		if (previousPath === undefined) delete process.env.PATH;
		else process.env.PATH = previousPath;
		if (previousAnthropicKey === undefined) delete process.env.ANTHROPIC_API_KEY;
		else process.env.ANTHROPIC_API_KEY = previousAnthropicKey;
		rmSync(bin, { recursive: true, force: true });
	}
});
