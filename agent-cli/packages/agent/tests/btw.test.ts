import assert from "node:assert/strict";
import test from "node:test";
import type { SessionEntry } from "@earendil-works/pi-coding-agent";

import { SESSION_ENTRY_TYPE } from "@cloudthinker/cloud/src/runtime.ts";
import { BTW_NEEDS_CONVERSATION, btwRequestHeaders } from "../src/btw-headers.ts";

const sessionWith = (entries: unknown[]) => ({ sessionManager: { getEntries: () => entries as SessionEntry[] } }) as Parameters<typeof btwRequestHeaders>[1];

test("btw side requests reach the gateway with the linked conversation, and fail clearly before a link", () => {
	const linked = sessionWith([{ type: "custom", customType: SESSION_ENTRY_TYPE, data: { conversation_id: "c-1" } }]);
	assert.deepEqual(btwRequestHeaders({ provider: "cloudthinker" }, linked), { "X-CloudThinker-Conversation": "c-1" });
	assert.throws(() => btwRequestHeaders({ provider: "cloudthinker" }, sessionWith([])), { message: BTW_NEEDS_CONVERSATION });
	assert.deepEqual(btwRequestHeaders({ provider: "other" }, sessionWith([])), {});
});
