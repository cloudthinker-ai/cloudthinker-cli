import assert from "node:assert/strict";
import test from "node:test";

import { CloudThinkerApiError } from "../src/client.ts";
import { classifyCloudError } from "../src/cloud-error.ts";

test("a 402 reads as out of credits, not as denied access", () => {
	const failure = classifyCloudError(new CloudThinkerApiError(402, "You're out of credits."));
	assert.equal(failure.kind, "out-of-credits");
	assert.equal(failure.retry, false);
});

test("a 403 still reads as denied access", () => {
	assert.equal(classifyCloudError(new CloudThinkerApiError(403, "forbidden")).kind, "denied");
});
