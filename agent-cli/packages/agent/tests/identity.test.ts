import assert from "node:assert/strict";
import test from "node:test";

import { buildSystemPrompt } from "../node_modules/@earendil-works/pi-coding-agent/dist/core/system-prompt.js";
import { CLOUDTHINKER_PREAMBLE, PI_PREAMBLE, withCloudThinkerIdentity } from "../src/identity.ts";

test("the model meets CloudThinker Agent, not pi's coding assistant or pi's docs", () => {
	const piPrompt = buildSystemPrompt({ cwd: "/work", selectedTools: ["read", "bash"], appendSystemPrompt: "Cloud addendum" });
	assert.ok(piPrompt.startsWith(PI_PREAMBLE), "pi changed its preamble; update PI_PREAMBLE so the identity swap still applies");
	assert.match(piPrompt, /<docs>\nPi documentation/, "pi changed its docs section; update the docs strip in identity.ts");
	const prompt = withCloudThinkerIdentity(piPrompt);
	assert.ok(prompt.startsWith(CLOUDTHINKER_PREAMBLE));
	assert.equal(prompt.includes("operating inside pi"), false);
	assert.doesNotMatch(prompt, /<docs>|Pi documentation|docs\/[\w-]+\.md|examples\/|README\.md/);
	assert.match(prompt, /<rules>\n[\s\S]*\n<\/rules>\n\n<addendum>\nCloud addendum\n<\/addendum>/);
	assert.equal(prompt.slice(CLOUDTHINKER_PREAMBLE.length), piPrompt.slice(PI_PREAMBLE.length).replace(/\n\n<docs>[\s\S]*?<\/docs>/, ""));
	assert.equal(withCloudThinkerIdentity("custom prompt"), "custom prompt");
});
