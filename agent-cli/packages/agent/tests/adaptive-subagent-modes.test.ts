import assert from "node:assert/strict";
import test from "node:test";

import { adaptiveSubagentModeGuidance } from "../src/subagent-modes.ts";

for (const ids of [["light", "pro", "ultra"], ["light", "standard"], []]) {
	test(`adaptive guidance exposes the advertised mode IDs: ${ids.join(", ") || "empty"}`, () => {
		const guidance = adaptiveSubagentModeGuidance(ids.map((id) => ({ provider: "cloudthinker", id })));
		const selectors = new Set([...guidance.matchAll(/cloudthinker\/[a-z]+/g)].map(([selector]) => selector));
		assert.deepEqual(selectors, new Set(ids.map((id) => `cloudthinker/${id}`)));
	});
}
