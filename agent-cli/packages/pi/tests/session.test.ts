import assert from "node:assert/strict";
import test from "node:test";

import type {
	ExtensionAPI,
	ExtensionContext,
	SessionEntry,
	SessionStartEvent,
} from "@earendil-works/pi-coding-agent";

import type { CloudThinkerClient, SessionCreated } from "../src/client.ts";
import type { HeaderState } from "../src/header.ts";
import {
	ASK_THREAD_ENTRY_TYPE,
	CloudThinkerRuntime,
	SESSION_ENTRY_TYPE,
} from "../src/runtime.ts";
import { findLinkedSession, linkSession, startSession } from "../src/session.ts";
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

function harness(client: Partial<CloudThinkerClient>, entries: SessionEntry[]) {
	const appended: Appended[] = [];
	const notes: { message: string; type: string | undefined }[] = [];
	const statuses: (string | undefined)[] = [];
	const headers: Partial<HeaderState>[] = [];
	const runtime = new CloudThinkerRuntime(
		{ appendEntry: (type: string, data: unknown) => appended.push({ type, data }) } as unknown as ExtensionAPI,
		client as CloudThinkerClient,
		hostVersionsFrom({ version: "0.4.0", piVersion: "0.85.1" }, "0.4.0"),
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
		custom(ASK_THREAD_ENTRY_TYPE, { conversation_id: "h-1", web_url: "http://web/h-1" }),
	];
	const { runtime, ctx, appended } = harness(client, entries);

	const linked = await linkSession(runtime, event("resume"), ctx);

	assert.deepEqual(linked, carried);
	assert.deepEqual(bodies, []);
	assert.deepEqual(runtime.autoMode, { enabled: true, canEdit: true });
	assert.deepEqual(runtime.askThread, { conversation_id: "h-1", web_url: "http://web/h-1" });
	assert.deepEqual(appended, []);
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

test("a session that cannot be opened marks the header unavailable and tells the developer", async () => {
	const client: Partial<CloudThinkerClient> = {
		createSession: async () => {
			throw new Error("gateway down");
		},
		whoami: async () => ({
			user_email: "dev@acme.io",
			workspace_id: WORKSPACE,
			workspace_name: "acme-prod",
			organization_id: null,
		}),
		getConnectionsContext: async () => ({ xml: "", prefixes: ["aws"] }),
	};
	const { runtime, ctx, notes, statuses, headers } = harness(client, []);

	await startSession(runtime, event("new"), ctx);

	assert.equal(runtime.session, undefined);
	assert.equal(runtime.identity?.workspace_name, "acme-prod");
	assert.deepEqual(runtime.connectedPrefixes, ["aws"]);
	assert.equal(headers.at(-1)?.link, "unavailable");
	assert.equal(headers.at(-1)?.workspaceName, "acme-prod");
	assert.deepEqual(statuses, ["✕ cloud unavailable"]);
	assert.equal(notes.length, 1);
	assert.equal(notes[0]?.type, "error");
	assert.match(notes[0]?.message ?? "", /gateway down/);
});
