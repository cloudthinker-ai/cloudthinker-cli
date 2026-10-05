import assert from "node:assert/strict";
import test from "node:test";

import type {
	ExtensionAPI,
	ExtensionContext,
	SessionEntry,
	SessionStartEvent,
} from "@earendil-works/pi-coding-agent";

import { CloudThinkerApiError, type CloudThinkerClient, type SessionCreated } from "../src/client.ts";
import type { HeaderState } from "../src/header.ts";
import {
	ASK_THREAD_ENTRY_TYPE,
	CloudThinkerRuntime,
	SESSION_ENTRY_TYPE,
} from "../src/runtime.ts";
import {
	findLinkedSession,
	findSelectedAgentReference,
	linkSession,
	retryLink,
	startLocalReviewSession,
	startSession,
} from "../src/session.ts";
import { hostVersionsFrom } from "../src/versions.ts";

const WORKSPACE = "22222222-2222-4222-8222-222222222222";

const carried: SessionCreated = {
	conversation_id: "c-1",
	workspace_id: WORKSPACE,
	web_url: "http://web/c-1",
	auto_mode: { enabled: true, can_edit: true },
};

const created: SessionCreated = {
	conversation_id: "c-2",
	workspace_id: WORKSPACE,
	web_url: "http://web/c-2",
	auto_mode: { enabled: false, can_edit: false },
};

function custom(customType: string, data: unknown): SessionEntry {
	return {
		type: "custom",
		id: `${customType}-${JSON.stringify(data).length}`,
		parentId: null,
		timestamp: "2026-09-07T00:00:00.000Z",
		customType,
		data,
	} as unknown as SessionEntry;
}

function event(reason: SessionStartEvent["reason"]): SessionStartEvent {
	return { type: "session_start", reason } as SessionStartEvent;
}

interface Appended {
	type: string;
	data: unknown;
}

function harness(
	client: Partial<CloudThinkerClient>,
	entries: SessionEntry[],
	selectedAgentReference?: string,
) {
	const appended: Appended[] = [];
	const notes: { message: string; type: string | undefined }[] = [];
	const statuses: (string | undefined)[] = [];
	const headers: Partial<HeaderState>[] = [];
	const runtime = new CloudThinkerRuntime(
		{ appendEntry: (type: string, data: unknown) => appended.push({ type, data }) } as unknown as ExtensionAPI,
		client as CloudThinkerClient,
		hostVersionsFrom({ version: "0.4.0", piVersion: "0.85.1" }, "0.4.0"),
		false,
		selectedAgentReference,
	);
	const originalSet = runtime.header.set.bind(runtime.header);
	runtime.header.set = (next) => {
		headers.push(next);
		originalSet(next);
	};
	const ctx = {
		cwd: "/tmp/repo",
		sessionManager: { getEntries: () => entries },
		ui: {
			setStatus: (_key: string, text: string | undefined) => statuses.push(text),
			notify: (message: string, type?: string) => notes.push({ message, type }),
			setWidget: () => {},
			setTitle: () => {},
		},
	} as unknown as ExtensionContext;
	runtime.bind(ctx);
	return { runtime, ctx, appended, notes, statuses, headers };
}

function creating(): { client: Partial<CloudThinkerClient>; bodies: unknown[] } {
	const bodies: unknown[] = [];
	return {
		bodies,
		client: {
			createSession: async (body: unknown) => {
				bodies.push(body);
				return created;
			},
		},
	};
}





test("a resumed session reuses the carried link and its stored mode and thread", async () => {
	const { client, bodies } = creating();
	const entries = [
		custom(SESSION_ENTRY_TYPE, carried),
		custom(ASK_THREAD_ENTRY_TYPE, {
			conversation_id: "h-1",
			web_url: "http://web/h-1",
			selected_agent_reference: "researcher",
		}),
	];
	const { runtime, ctx, appended } = harness(client, entries);

	const linked = await linkSession(runtime, event("resume"), ctx);

	assert.deepEqual(linked, carried);
	assert.deepEqual(bodies, []);
	assert.deepEqual(runtime.autoMode, { enabled: true, canEdit: true });
	assert.deepEqual(runtime.askThread, {
		conversation_id: "h-1",
		web_url: "http://web/h-1",
		selected_agent_reference: "researcher",
	});
	assert.equal(runtime.selectedAgentReference, "researcher");
	assert.deepEqual(appended, []);
});

test("a linked custom selection is available to child CloudThinker sessions", () => {
	const entries = [
		custom(SESSION_ENTRY_TYPE, {
			...carried,
			selected_agent_reference: "  custom-agent  ",
		}),
	];
	assert.equal(findSelectedAgentReference(entries), "custom-agent");
});

test("a configured custom selection supersedes a carried identity and is persisted", async () => {
	const { client } = creating();
	const entries = [custom(SESSION_ENTRY_TYPE, carried)];
	const { runtime, ctx, appended } = harness(client, entries, "researcher");

	await linkSession(runtime, event("resume"), ctx);

	assert.equal(runtime.selectedAgentReference, "researcher");
	assert.equal(runtime.session?.selected_agent_reference, "researcher");
	assert.deepEqual(appended, [{
		type: SESSION_ENTRY_TYPE,
		data: { ...carried, selected_agent_reference: "researcher" },
	}]);
});

test("local review links a generic gateway session without sending the checkout path", async () => {
	const { client, bodies } = creating();
	const { runtime, appended } = harness(client, []);

	const session = await startLocalReviewSession(runtime);

	assert.deepEqual(bodies, [{ cwd: "local-review", title: "Local code review", skip_sandbox_warmup: true }]);
	assert.deepEqual(session, created);
	assert.deepEqual(appended, [{ type: SESSION_ENTRY_TYPE, data: created }]);
	assert.equal(runtime.session, created);
});

test("a fork opens a new conversation sourced from the carried one and drops the thread", async () => {
	const { client, bodies } = creating();
	const entries = [
		custom(SESSION_ENTRY_TYPE, carried),
		custom(ASK_THREAD_ENTRY_TYPE, { conversation_id: "h-1", web_url: "http://web/h-1" }),
	];
	const { runtime, ctx, appended } = harness(client, entries);

	const linked = await linkSession(runtime, event("fork"), ctx);

	assert.deepEqual(linked, created);
	assert.deepEqual(bodies, [{ cwd: "/tmp/repo", source_conversation_id: "c-1" }]);
	assert.equal(runtime.askThread, undefined);
	assert.deepEqual(runtime.autoMode, { enabled: false, canEdit: false });
	assert.deepEqual(appended, [{ type: SESSION_ENTRY_TYPE, data: created }]);
});

test("a session with nothing carried is created fresh", async () => {
	const { client, bodies } = creating();
	const { runtime, ctx, appended } = harness(client, []);

	await linkSession(runtime, event("new"), ctx);

	assert.deepEqual(bodies, [{ cwd: "/tmp/repo", source_conversation_id: undefined }]);
	assert.deepEqual(runtime.session, created);
	assert.equal(appended.length, 1);
});

test("a created session hydrates the resolved custom-agent identity", async () => {
	const resolved = { ...created, selected_agent_reference: "agent-id" } satisfies SessionCreated;
	const { bodies } = creating();
	const { runtime, ctx, appended } = harness({
		createSession: async (body: unknown) => {
			bodies.push(body);
			return resolved;
		},
	}, []);

	await linkSession(runtime, event("new"), ctx);

	assert.equal(runtime.selectedAgentReference, "agent-id");
	assert.equal(runtime.session?.selected_agent_reference, "agent-id");
	assert.deepEqual(appended, [{ type: SESSION_ENTRY_TYPE, data: resolved }]);
});

test("an explicit custom-agent identity is sent when opening a session", async () => {
	const { client, bodies } = creating();
	const { runtime, ctx } = harness(client, [], "cost-helper");

	await linkSession(runtime, event("new"), ctx);

	assert.deepEqual(bodies, [{
		cwd: "/tmp/repo",
		source_conversation_id: undefined,
		selected_agent_reference: "cost-helper",
	}]);
});

test("a linked session enables ct_ask independently of specialists", async () => {
	const { bodies } = creating();
	const explicit = { ...created } satisfies SessionCreated;
	const { runtime, ctx } = harness({
		createSession: async (body: unknown) => {
			bodies.push(body);
			return explicit;
		},
	}, []);

	await linkSession(runtime, event("new"), ctx);

});

test("a changed approval mode is written to the session so a resume reads the latest", async () => {
	const { client } = creating();
	const entries = [custom(SESSION_ENTRY_TYPE, carried)];
	const { runtime, ctx, appended } = harness(client, entries);
	await linkSession(runtime, event("resume"), ctx);

	runtime.setAutoMode({ enabled: true, canEdit: true });
	assert.equal(appended.length, 0);

	runtime.setAutoMode({ enabled: false, canEdit: true });
	assert.equal(appended.length, 1);
	const written = appended[0];
	assert.equal(written?.type, SESSION_ENTRY_TYPE);
	assert.equal(runtime.session?.auto_mode.enabled, false);

	const resumed = findLinkedSession([...entries, custom(SESSION_ENTRY_TYPE, written?.data)]);
	assert.equal(resumed?.conversation_id, "c-1");
	assert.deepEqual(resumed?.auto_mode, { enabled: false, can_edit: true });
});

test("a session that cannot be opened says why, retries a reachable failure, and announces recovery", async () => {
	let failure: Error | undefined = new CloudThinkerApiError(0, "Could not reach http://localhost:8000: fetch failed");
	const client: Partial<CloudThinkerClient> = {
		createSession: async () => {
			if (failure) throw failure;
			return created;
		},
		whoami: async () => ({
			user_email: "dev@acme.io",
			workspace_id: WORKSPACE,
			workspace_name: "acme-prod",
			organization_id: null,
		}),
		getConnectionsContext: async () => ({ xml: "", prefixes: ["aws"] }),
		listModels: async () => [{ id: "pro", name: "Pro", reasoning: true, input: ["text"], contextWindow: 200_000, maxTokens: 32_000 }] as never,
	};
	const { runtime, ctx, notes, statuses, headers } = harness(client, []);
	const providers: string[] = [];
	const selected: unknown[] = [];
	Object.assign(runtime.pi, {
		registerProvider: (id: string) => providers.push(id),
		setModel: async (model: unknown) => selected.push(model),
	});
	Object.assign(ctx, { model: undefined, modelRegistry: { find: (provider: string, id: string) => ({ provider, id }) } });

	await startSession(runtime, event("new"), ctx);

	assert.equal(runtime.session, undefined);
	assert.equal(runtime.identity?.workspace_name, "acme-prod");
	assert.deepEqual(runtime.connectedPrefixes, ["aws"]);
	assert.equal(headers.at(-1)?.link, "unavailable");
	assert.equal(runtime.linkFailure?.label, "offline: can't reach localhost:8000");
	assert.deepEqual(statuses, ["retrying in 5s · /cloud retry to try now"]);
	assert.equal(notes.length, 1);
	assert.equal(notes[0]?.type, "error");
	assert.match(notes[0]?.message ?? "", /can't reach localhost:8000[\s\S]*Retrying automatically[\s\S]*fetch failed/);

	failure = undefined;
	await retryLink(runtime, ctx);
	assert.equal(runtime.session, created);
	assert.equal(runtime.linkFailure === undefined, true);
	assert.equal(headers.at(-1)?.link, "linked");
	assert.equal(statuses.at(-1), undefined);
	assert.match(notes.at(-1)?.message ?? "", /cloud is back/);
	assert.deepEqual(providers, ["cloudthinker"]);
	assert.deepEqual(selected, [{ provider: "cloudthinker", id: "pro" }]);

	runtime.reset();
	failure = new CloudThinkerApiError(401, "`cloudthinker auth token` exited with code 3. Run `cloudthinker login`.");
	await startSession(runtime, event("new"), ctx);
	assert.equal(runtime.linkFailure?.label, "signed out");
	assert.equal(statuses.at(-1), "run `cloudthinker login` in a terminal, then /cloud retry");

	runtime.reset();
	failure = new CloudThinkerApiError(503, "Service Unavailable");
	await startSession(runtime, event("new"), ctx);
	assert.equal(statuses.at(-1), "retrying in 5s · /cloud retry to try now");
	Object.assign(runtime.pi, { getActiveTools: () => [], setActiveTools: () => {} });
	runtime.setCloudEnabled(false, false);
	assert.equal(runtime.linkFailure === undefined, true);
	assert.equal(statuses.at(-2), undefined);
	runtime.cancelRetry();
});

test("concurrent link attempts share one cloud session, and a closed runtime never retries", async () => {
	let calls = 0;
	let release: (() => void) | undefined;
	const client: Partial<CloudThinkerClient> = {
		createSession: async () => {
			calls += 1;
			await new Promise<void>((resolve) => { release = resolve; });
			return created;
		},
		whoami: async () => ({ user_email: "primary@example.com", workspace_id: WORKSPACE, workspace_name: "primary", organization_id: null }),
		getConnectionsContext: async () => ({ xml: "", prefixes: [] }),
	};
	const { runtime, ctx, appended } = harness(client, []);
	runtime.models = [{ id: "pro" } as never];
	const first = retryLink(runtime, ctx);
	const second = linkSession(runtime, event("new"), ctx);
	await new Promise((resolve) => setImmediate(resolve));
	release?.();
	await Promise.all([first, second]);
	assert.equal(calls, 1);
	assert.equal(appended.filter((entry) => entry.type === "cloudthinker").length, 1);

	runtime.reset();
	runtime.closed = true;
	await retryLink(runtime, ctx);
	assert.equal(calls, 1);
});
