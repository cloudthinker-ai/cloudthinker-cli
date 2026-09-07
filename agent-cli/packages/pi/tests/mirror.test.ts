import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import type { SessionEntry } from "@earendil-works/pi-coding-agent";

import { CloudThinkerClient, TokenSource, type MirrorEntryInput } from "../src/client.ts";
import {
	MAX_ENTRIES_PER_BATCH,
	SessionMirror,
	batch,
	formatMirrorStatus,
	payloadBytes,
	toMirrorEntry,
} from "../src/mirror.ts";
import { outboxPath, rejectedPath } from "../src/paths.ts";
import { startFakeServer, withTempDir } from "./helpers.ts";

const CONVERSATION = "11111111-1111-1111-1111-111111111111";

function entry(id: string, parentId: string | null, text = "hi"): SessionEntry {
	return {
		type: "message",
		id,
		parentId,
		timestamp: "2026-09-06T00:00:00.000Z",
		message: { role: "user", content: text },
	} as unknown as SessionEntry;
}

function clientFor(origin: string): CloudThinkerClient {
	return new CloudThinkerClient({
		baseUrl: origin,
		tokens: new TokenSource({ CLOUDTHINKER_TOKEN: "t" }),
	});
}

test("an entry maps onto pi's own id, parent, and type with the entry as payload", () => {
	const mapped = toMirrorEntry(entry("e2", "e1"));
	assert.equal(mapped.entry_id, "e2");
	assert.equal(mapped.parent_id, "e1");
	assert.equal(mapped.entry_type, "message");
	assert.deepEqual(mapped.payload, entry("e2", "e1"));
});

test("batches never exceed the append cap", () => {
	const batches = batch(Array.from({ length: 450 }, (_, i) => i), MAX_ENTRIES_PER_BATCH);
	assert.deepEqual(
		batches.map((chunk) => chunk.length),
		[200, 200, 50],
	);
});

test("only unacknowledged entries are sent, oldest first, in capped batches", async () => {
	const appended: MirrorEntryInput[][] = [];
	const server = await startFakeServer((request) => {
		if (request.method === "GET") return { body: { entries: [], last_seq: 0 } };
		const body = request.body as { entries: MirrorEntryInput[] };
		appended.push(body.entries);
		return { body: { stored: body.entries.length, last_seq: body.entries.length } };
	});
	try {
		await withTempDir(async (dir) => {
			const mirror = new SessionMirror(clientFor(server.origin), () => {}, dir);
			await mirror.link(CONVERSATION);
			const entries = Array.from({ length: 250 }, (_, index) =>
				entry(`e${index}`, index === 0 ? null : `e${index - 1}`),
			);
			await mirror.flush(entries);
			assert.deepEqual(
				appended.map((chunk) => chunk.length),
				[200, 50],
			);
			assert.equal(appended[0]?.[0]?.entry_id, "e0");
			assert.equal(appended[1]?.[49]?.entry_id, "e249");

			appended.length = 0;
			await mirror.flush([...entries, entry("e250", "e249")]);
			assert.deepEqual(
				appended.map((chunk) => chunk.map((item) => item.entry_id)),
				[["e250"]],
			);
		});
	} finally {
		await server.close();
	}
});

test("link seeds the acknowledged set so a resume re-sends only its tail", async () => {
	const appended: MirrorEntryInput[][] = [];
	const server = await startFakeServer((request) => {
		if (request.method === "GET") {
			return {
				body: {
					entries: [
						{ seq: 1, entry_id: "e0", parent_id: null, entry_type: "message", payload: {} },
					],
					last_seq: 1,
				},
			};
		}
		const body = request.body as { entries: MirrorEntryInput[] };
		appended.push(body.entries);
		return { body: { stored: body.entries.length, last_seq: 2 } };
	});
	try {
		await withTempDir(async (dir) => {
			const mirror = new SessionMirror(clientFor(server.origin), () => {}, dir);
			await mirror.link(CONVERSATION);
			await mirror.flush([entry("e0", null), entry("e1", "e0")]);
			assert.deepEqual(
				appended.map((chunk) => chunk.map((item) => item.entry_id)),
				[["e1"]],
			);
		});
	} finally {
		await server.close();
	}
});

test("a rejected batch lands in the outbox and the next run delivers it", async () => {
	let failNext = true;
	const appended: MirrorEntryInput[][] = [];
	const server = await startFakeServer((request) => {
		if (request.method === "GET") return { body: { entries: [], last_seq: 0 } };
		if (failNext) {
			failNext = false;
			return { status: 503, body: { detail: "gateway down" } };
		}
		const body = request.body as { entries: MirrorEntryInput[] };
		appended.push(body.entries);
		return { body: { stored: body.entries.length, last_seq: body.entries.length } };
	});
	try {
		await withTempDir(async (dir) => {
			let clock = 1_000;
			const statuses: (string | undefined)[] = [];
			const mirror = new SessionMirror(
				clientFor(server.origin),
				(status) => statuses.push(formatMirrorStatus(status)),
				dir,
				() => clock,
			);
			await mirror.link(CONVERSATION);
			const entries = [entry("e0", null), entry("e1", "e0")];

			await assert.rejects(() => mirror.flush(entries));
			const written = await readFile(outboxPath(CONVERSATION, dir), "utf8");
			assert.deepEqual(
				written
					.trim()
					.split("\n")
					.map((line) => (JSON.parse(line) as MirrorEntryInput).entry_id),
				["e0", "e1"],
			);
			assert.ok(statuses.some((line) => line?.includes("mirror offline, 2 pending")));

			await mirror.flush(entries);
			assert.deepEqual(
				appended.map((chunk) => chunk.map((item) => item.entry_id)),
				[],
			);

			clock += 5_000;
			await mirror.flush(entries);
			assert.deepEqual(
				appended.map((chunk) => chunk.map((item) => item.entry_id)),
				[["e0", "e1"]],
			);
			await assert.rejects(() => readFile(outboxPath(CONVERSATION, dir), "utf8"));
			assert.ok(statuses.length > 0);
			assert.equal(statuses.at(-1), undefined);
		});
	} finally {
		await server.close();
	}
});

test("an oversized payload ships truncated so the parent walk stays unbroken", async () => {
	const appended: MirrorEntryInput[][] = [];
	const server = await startFakeServer((request) => {
		if (request.method === "GET") return { body: { entries: [], last_seq: 0 } };
		const body = request.body as { entries: MirrorEntryInput[] };
		appended.push(body.entries);
		return { body: { stored: body.entries.length, last_seq: body.entries.length } };
	});
	try {
		await withTempDir(async (dir) => {
			const statuses: (string | undefined)[] = [];
			const mirror = new SessionMirror(
				clientFor(server.origin),
				(status) => statuses.push(formatMirrorStatus(status)),
				dir,
			);
			await mirror.link(CONVERSATION);
			const huge = entry("big", "e0", "x".repeat(1024 * 1024 + 10));
			assert.ok(payloadBytes(toMirrorEntry(huge)) > 1024 * 1024);
			await mirror.flush([huge, entry("small", "big")]);

			assert.deepEqual(
				appended.map((chunk) => chunk.map((item) => item.entry_id)),
				[["big", "small"]],
			);
			const sent = appended[0]?.[0];
			assert.equal(sent?.parent_id, "e0");
			assert.equal(sent?.entry_type, "message");
			assert.deepEqual(sent?.payload, {
				type: "message",
				id: "big",
				parentId: "e0",
				timestamp: "2026-09-06T00:00:00.000Z",
				truncated: true,
			});
			assert.ok(payloadBytes(sent as MirrorEntryInput) < 1024);
			assert.ok(statuses.at(-1)?.includes("1 truncated"));
		});
	} finally {
		await server.close();
	}
});

test("a terminal 4xx drops its chunk without the outbox and keeps the rest going", async () => {
	const appended: MirrorEntryInput[][] = [];
	const server = await startFakeServer((request) => {
		if (request.method === "GET") return { body: { entries: [], last_seq: 0 } };
		const body = request.body as { entries: MirrorEntryInput[] };
		if (body.entries.some((item) => item.entry_id === "e0")) {
			return { status: 422, body: { detail: "entry_type is not known" } };
		}
		appended.push(body.entries);
		return { body: { stored: body.entries.length, last_seq: body.entries.length } };
	});
	try {
		await withTempDir(async (dir) => {
			const statuses: (string | undefined)[] = [];
			const mirror = new SessionMirror(
				clientFor(server.origin),
				(status) => statuses.push(formatMirrorStatus(status)),
				dir,
			);
			await mirror.link(CONVERSATION);
			const entries = Array.from({ length: 250 }, (_, index) =>
				entry(`e${index}`, index === 0 ? null : `e${index - 1}`),
			);

			await mirror.flush(entries);
			assert.deepEqual(
				appended.map((chunk) => chunk.length),
				[50],
			);
			assert.equal(appended[0]?.[0]?.entry_id, "e200");
			await assert.rejects(() => readFile(outboxPath(CONVERSATION, dir), "utf8"));
			assert.ok(statuses.at(-1)?.includes("200 rejected"));

			appended.length = 0;
			await mirror.flush(entries);
			assert.deepEqual(appended, []);
		});
	} finally {
		await server.close();
	}
});

test("a 429 keeps the outbox and backoff path", async () => {
	const server = await startFakeServer((request) => {
		if (request.method === "GET") return { body: { entries: [], last_seq: 0 } };
		return { status: 429, body: { detail: "slow down" } };
	});
	try {
		await withTempDir(async (dir) => {
			const statuses: (string | undefined)[] = [];
			const mirror = new SessionMirror(
				clientFor(server.origin),
				(status) => statuses.push(formatMirrorStatus(status)),
				dir,
				() => 1_000,
			);
			await mirror.link(CONVERSATION);
			await assert.rejects(() => mirror.flush([entry("e0", null)]));

			const written = await readFile(outboxPath(CONVERSATION, dir), "utf8");
			assert.ok(written.includes("e0"));
			assert.ok(statuses.at(-1)?.includes("mirror offline, 1 pending"));
			assert.ok(!statuses.at(-1)?.includes("rejected"));
		});
	} finally {
		await server.close();
	}
});

test("a failed link reports the mirror offline and a later flush links and delivers", async () => {
	let listingDown = true;
	const appended: MirrorEntryInput[][] = [];
	const server = await startFakeServer((request) => {
		if (request.method === "GET") {
			if (listingDown) return { status: 503, body: { detail: "down" } };
			return { body: { entries: [], last_seq: 0 } };
		}
		const body = request.body as { entries: MirrorEntryInput[] };
		appended.push(body.entries);
		return { body: { stored: body.entries.length, last_seq: body.entries.length } };
	});
	try {
		await withTempDir(async (dir) => {
			let clock = 1_000;
			const statuses: (string | undefined)[] = [];
			const mirror = new SessionMirror(
				clientFor(server.origin),
				(status) => statuses.push(formatMirrorStatus(status)),
				dir,
				() => clock,
			);
			await assert.rejects(() => mirror.link(CONVERSATION));
			assert.ok(statuses.at(-1)?.includes("mirror offline"));

			const entries = [entry("e0", null), entry("e1", "e0")];
			const requestsBefore = server.requests.length;
			await mirror.flush(entries);
			assert.equal(server.requests.length, requestsBefore);
			assert.ok(statuses.at(-1)?.includes("mirror offline, 2 pending"));

			listingDown = false;
			clock += 5_000;
			await mirror.flush(entries);
			assert.deepEqual(
				appended.map((chunk) => chunk.map((item) => item.entry_id)),
				[["e0", "e1"]],
			);
			assert.equal(statuses.at(-1), undefined);
		});
	} finally {
		await server.close();
	}
});

test("a terminal rejection survives a restart, so the chunk is never re-sent", async () => {
	const accepted: string[] = [];
	const rejectedPosts: number[] = [];
	const server = await startFakeServer((request) => {
		if (request.method === "GET") {
			return {
				body: {
					entries: accepted.map((entry_id, index) => ({
						seq: index + 1,
						entry_id,
						parent_id: null,
						entry_type: "message",
						payload: {},
					})),
					last_seq: accepted.length,
				},
			};
		}
		const body = request.body as { entries: MirrorEntryInput[] };
		if (body.entries.some((item) => item.entry_id === "e0")) {
			rejectedPosts.push(body.entries.length);
			return { status: 422, body: { detail: "entry_type is not known" } };
		}
		for (const item of body.entries) accepted.push(item.entry_id);
		return { body: { stored: body.entries.length, last_seq: accepted.length } };
	});
	try {
		await withTempDir(async (dir) => {
			const entries = Array.from({ length: 250 }, (_, index) =>
				entry(`e${index}`, index === 0 ? null : `e${index - 1}`),
			);
			const first = new SessionMirror(clientFor(server.origin), () => {}, dir);
			await first.link(CONVERSATION);
			await first.flush(entries);
			assert.deepEqual(rejectedPosts, [200]);
			assert.deepEqual(
				JSON.parse(await readFile(rejectedPath(CONVERSATION, dir), "utf8")).length,
				200,
			);

			const statuses: (string | undefined)[] = [];
			const restarted = new SessionMirror(
				clientFor(server.origin),
				(status) => statuses.push(formatMirrorStatus(status)),
				dir,
			);
			await restarted.link(CONVERSATION);
			await restarted.flush(entries);
			assert.deepEqual(rejectedPosts, [200]);
			assert.equal(accepted.length, 50);
			assert.ok(statuses.at(-1)?.includes("200 rejected"));
			assert.ok(!statuses.at(-1)?.includes("pending"));
		});
	} finally {
		await server.close();
	}
});
