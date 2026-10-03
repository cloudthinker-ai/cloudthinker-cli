import assert from "node:assert/strict";
import test from "node:test";

import type { ExtensionAPI, ExtensionCommandContext } from "@earendil-works/pi-coding-agent";

import type { AgentCliWorkspace, CloudThinkerClient, GatewayModel } from "../src/client.ts";
import { CloudThinkerRuntime } from "../src/runtime.ts";
import { hostVersionsFrom } from "../src/versions.ts";
import { TOKEN_PINS_WORKSPACE, workspaceCommand } from "../src/workspace.ts";

const ACME = "11111111-1111-4111-8111-111111111111";
const BETA = "22222222-2222-4222-8222-222222222222";
const GAMMA = "33333333-3333-4333-8333-333333333333";
const MINE = "44444444-4444-4444-8444-444444444444";
const STATUS = JSON.stringify({
	host: "https://app.cloudthinker.io",
	workspaces: [
		{ workspace_id: ACME, workspace_name: "Prod", active: true },
		{ workspace_id: BETA, workspace_name: "Staging", active: false },
	],
});
const REMOTE: AgentCliWorkspace[] = [
	{ workspace_id: ACME, workspace_name: "Prod", organization_id: "o1", organization_name: "Acme Corp", current: true },
	{ workspace_id: BETA, workspace_name: "Staging", organization_id: "o2", organization_name: "Beta Inc", current: false },
	{ workspace_id: GAMMA, workspace_name: "Dev", organization_id: "o2", organization_name: "Beta Inc", current: false },
	{ workspace_id: MINE, workspace_name: "Mine", organization_id: null, organization_name: null, current: false },
];

function harness(selections: string[], remote: AgentCliWorkspace[] | undefined) {
	const registered: { apiKey?: string }[] = [];
	const commands: string[][] = [];
	const notices: string[] = [];
	const picks: string[][] = [];
	let newSessions = 0;
	const pi = { registerProvider: (_name: string, config: { apiKey?: string }) => registered.push(config) };
	const client = {
		listModels: async () => [{ id: "pro", name: "pro" } as unknown as GatewayModel],
		listWorkspaces: async () => remote,
		invalidateTokens: () => {},
	};
	const runtime = new CloudThinkerRuntime(
		pi as unknown as ExtensionAPI,
		client as unknown as CloudThinkerClient,
		hostVersionsFrom({ version: "0.4.0", piVersion: "0.85.1" }, "0.4.0"),
	);
	runtime.models = [{ id: "pro", name: "pro" } as unknown as GatewayModel];
	runtime.session = { conversation_id: "c-1", workspace_id: ACME, web_url: "https://web/c-1" } as never;
	const queue = [...selections];
	const ui = {
		notify: (message: string) => notices.push(message),
		select: async (_title: string, options: string[]) => {
			picks.push(options);
			return queue.shift();
		},
	};
	const ctx = {
		ui,
		isIdle: () => true,
		newSession: async (options?: { withSession?: (next: { ui: typeof ui }) => Promise<void> }) => {
			newSessions += 1;
			await options?.withSession?.({ ui });
			return { cancelled: false };
		},
	} as unknown as ExtensionCommandContext;
	const run = async (_command: string, args: string[]) => {
		commands.push(args);
		return { status: 0, stdout: args.includes("status") ? STATUS : "" };
	};
	return { runtime, ctx, run, registered, commands, notices, picks, newSessions: () => newSessions };
}

async function withEnv(values: Record<string, string | undefined>, body: (env: NodeJS.ProcessEnv) => Promise<void>) {
	const env = process.env;
	const saved = Object.fromEntries(Object.keys(values).map((key) => [key, env[key]]));
	for (const [key, value] of Object.entries(values)) {
		if (value === undefined) delete env[key];
		else env[key] = value;
	}
	try {
		await body(env);
	} finally {
		for (const [key, value] of Object.entries(saved)) {
			if (value === undefined) delete env[key];
			else env[key] = value;
		}
	}
}

const APP = { CLOUDTHINKER_URL: "https://app.cloudthinker.io", CLOUDTHINKER_TOKEN: undefined };

test("workspace lists every workspace grouped by organization, then switches to the one picked", async () => {
	await withEnv({ ...APP, CLOUDTHINKER_WORKSPACE: ACME }, async (env) => {
		const h = harness(["Beta Inc", "  Staging"], REMOTE);
		await workspaceCommand(h.runtime, h.ctx, "", { run: h.run, env });
		assert.deepEqual(h.picks[0], [
			"Acme Corp",
			"  Prod  (current)",
			"Beta Inc",
			"  Dev  · login needed",
			"  Staging",
			"Personal",
			"  Mine  · login needed",
		]);
		assert.equal(h.picks.length, 2);
		assert.equal(env.CLOUDTHINKER_WORKSPACE, BETA);
		assert.match(h.registered.at(-1)?.apiKey ?? "", new RegExp(`--workspace ${BETA}$`));
		assert.deepEqual(h.commands.at(-1), ["--url", "https://app.cloudthinker.io", "auth", "switch", BETA]);
		assert.equal(h.newSessions(), 1);
		assert.deepEqual(h.notices, ["Switched to Staging. This is a new session in that workspace."]);
	});
});

test("a workspace without a login points at cloudthinker login and changes nothing; an older server falls back to the logged-in list", async () => {
	await withEnv({ ...APP, CLOUDTHINKER_WORKSPACE: ACME }, async (env) => {
		const h = harness(["  Dev  · login needed"], REMOTE);
		await workspaceCommand(h.runtime, h.ctx, "", { run: h.run, env });
		assert.match(h.notices[0] ?? "", /not logged in to Dev in Beta Inc.*cloudthinker login --url https:\/\/app\.cloudthinker\.io/);
		assert.equal(env.CLOUDTHINKER_WORKSPACE, ACME);
		assert.equal(h.newSessions(), 0);

		const old = harness([], undefined);
		await workspaceCommand(old.runtime, old.ctx, "", { run: old.run, env });
		assert.deepEqual(old.picks[0], ["Logged in", "  Prod  (current)", "  Staging"]);
	});
});

test("workspace refuses to switch when an environment token pins the session, and names an unknown workspace", async () => {
	await withEnv({ CLOUDTHINKER_TOKEN: "t" }, async (env) => {
		const pinned = harness([], REMOTE);
		await workspaceCommand(pinned.runtime, pinned.ctx, "Staging", { run: pinned.run, env });
		assert.deepEqual(pinned.notices, [TOKEN_PINS_WORKSPACE]);
		assert.equal(pinned.newSessions(), 0);
	});
	await withEnv({ ...APP, CLOUDTHINKER_WORKSPACE: ACME }, async (env) => {
		const unknown = harness([], REMOTE);
		await workspaceCommand(unknown.runtime, unknown.ctx, "Nope", { run: unknown.run, env });
		assert.match(unknown.notices[0] ?? "", /No workspace matches "Nope"/);
		assert.equal(env.CLOUDTHINKER_WORKSPACE, ACME);
	});
});
