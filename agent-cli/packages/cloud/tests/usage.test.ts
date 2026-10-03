import assert from "node:assert/strict";
import test from "node:test";

import type { SessionEntry } from "@earendil-works/pi-coding-agent";

import { sessionPanelLines, sessionTotals } from "../src/usage.ts";

function assistant(usage: Record<string, number>, toolCalls = 0): SessionEntry {
	return {
		type: "message",
		message: {
			role: "assistant",
			content: Array.from({ length: toolCalls }, () => ({ type: "toolCall" })),
			usage,
		},
	} as unknown as SessionEntry;
}

const ENTRIES: SessionEntry[] = [
	{ type: "message", message: { role: "user", content: "hi" } } as unknown as SessionEntry,
	assistant({ input: 100, output: 20, cacheRead: 400, cacheWrite: 50 }, 1),
	{
		type: "message",
		message: { role: "toolResult", usage: { input: 5, output: 1, cacheRead: 0, cacheWrite: 0 } },
	} as unknown as SessionEntry,
	{
		type: "compaction",
		usage: { input: 10, output: 2, cacheRead: 0, cacheWrite: 0 },
	} as unknown as SessionEntry,
];

test("session totals count messages and every billed token", () => {
	const totals = sessionTotals(ENTRIES);

	assert.deepEqual(totals, {
		messages: 3,
		user: 1,
		assistant: 1,
		toolCalls: 1,
		toolResults: 1,
		input: 115,
		output: 23,
		cacheRead: 400,
		cacheWrite: 50,
	});
});

test("the panel prices the session in credits and never in a currency", () => {
	const lines = sessionPanelLines(
		{ file: "/tmp/s.jsonl", id: "abc", name: undefined, webUrl: "https://ct.test/chat/1" },
		sessionTotals(ENTRIES),
		{ credits_used: 1.75, tokens_consumed: 87_500 },
	);
	const text = lines.join("\n");

	assert.ok(!text.includes("$"));
	assert.ok(text.includes("Charged: ◆ 1.75 credits"));
	assert.ok(text.includes("Billed by the gateway: 87,500 tokens"));
	assert.ok(text.includes("Input: 565"));
	assert.ok(text.includes("Total: 588"));
	assert.ok(text.includes("Mirror: https://ct.test/chat/1"));
});

test("a session the ledger has not answered for says so", () => {
	const lines = sessionPanelLines(
		{ file: undefined, id: "abc", name: "infra", webUrl: undefined },
		sessionTotals([]),
		undefined,
	);

	assert.ok(lines.includes("Name: infra"));
	assert.ok(lines.includes("File: in memory"));
	assert.ok(lines.includes("Mirror: not linked"));
	assert.ok(lines.includes("This workspace's ledger has not answered yet."));
});
