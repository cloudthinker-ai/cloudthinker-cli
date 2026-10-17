import assert from "node:assert/strict";
import { join } from "node:path";
import test from "node:test";

import { outboxPath, rejectedPath } from "../src/paths.ts";

test("mirror filenames accept safe ids and reject path segments", () => {
	const agentDir = "/tmp/agent";
	assert.equal(outboxPath("c-1", agentDir), join(agentDir, "outbox", "c-1.jsonl"));
	assert.equal(
		rejectedPath("c-1", agentDir),
		join(agentDir, "outbox", "c-1.rejected.json"),
	);
	assert.throws(() => outboxPath("../outside", agentDir), /only letters/);
	assert.throws(() => rejectedPath("folder/session", agentDir), /only letters/);
	assert.throws(() => rejectedPath("folder\\session", agentDir), /only letters/);
	assert.throws(() => outboxPath("bad.id", agentDir), /only letters/);
});
