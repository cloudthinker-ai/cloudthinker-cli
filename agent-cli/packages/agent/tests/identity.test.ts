import assert from "node:assert/strict";
import test from "node:test";

import { buildSystemPrompt } from "../node_modules/@earendil-works/pi-coding-agent/dist/core/system-prompt.js";
import { CLOUDTHINKER_PREAMBLE, PI_PREAMBLE, withCloudThinkerIdentity } from "../src/identity.ts";

test("the model meets CloudThinker Agent, not pi's coding assistant", () => {
	const piPrompt = buildSystemPrompt({ cwd: "/work", selectedTools: ["read", "bash"] });
	assert.ok(piPrompt.startsWith(PI_PREAMBLE), "pi changed its preamble; update PI_PREAMBLE so the identity swap still applies");
	const prompt = withCloudThinkerIdentity(piPrompt);
	assert.ok(prompt.startsWith(CLOUDTHINKER_PREAMBLE));
	assert.equal(prompt.includes("operating inside pi"), false);
	assert.equal(prompt.slice(CLOUDTHINKER_PREAMBLE.length), piPrompt.slice(PI_PREAMBLE.length));
	assert.equal(withCloudThinkerIdentity("custom prompt"), "custom prompt");
});
