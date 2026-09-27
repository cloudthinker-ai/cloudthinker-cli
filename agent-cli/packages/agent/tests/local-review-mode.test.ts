import assert from "node:assert/strict";
import test from "node:test";

import {
	LOCAL_REVIEW_FLAG,
	isLocalReview,
	withoutLocalReviewFlag,
} from "../src/local-review-mode.ts";

test("the internal review flag selects local review mode and is removed before pi parses arguments", () => {
	const args = ["--print", LOCAL_REVIEW_FLAG, "-p", "Review this diff"];
	assert.equal(isLocalReview(args), true);
	assert.deepEqual(withoutLocalReviewFlag(args), ["--print", "-p", "Review this diff"]);
});

test("ordinary agent arguments keep the standard mode", () => {
	const args = ["--print", "-p", "hello"];
	assert.equal(isLocalReview(args), false);
	assert.deepEqual(withoutLocalReviewFlag(args), args);
});
