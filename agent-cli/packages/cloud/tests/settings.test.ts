import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { CLOUD_DEFAULT_KEY, cloudDefaultEnabled, resolveCloudEnabled } from "../src/settings.ts";

function project(run: (dir: string) => void, settings?: string): void {
	const root = mkdtempSync(join(tmpdir(), "ct-cloud-default-"));
	const previous = process.env.PI_CODING_AGENT_DIR;
	process.env.PI_CODING_AGENT_DIR = join(root, "agent");
	try {
		if (settings !== undefined) {
			mkdirSync(join(root, ".pi"), { recursive: true });
			writeFileSync(join(root, ".pi", "settings.json"), settings);
		}
		run(root);
	} finally {
		if (previous === undefined) delete process.env.PI_CODING_AGENT_DIR;
		else process.env.PI_CODING_AGENT_DIR = previous;
		rmSync(root, { recursive: true, force: true });
	}
}

test("CA-CLOUD-5: a false cloudDefault reads as Cloud off", () => {
	project((dir) => assert.equal(cloudDefaultEnabled(dir), false), JSON.stringify({ [CLOUD_DEFAULT_KEY]: false }));
});

test("CA-CLOUD-6: an absent or true cloudDefault keeps today's Cloud on", () => {
	project((dir) => assert.equal(cloudDefaultEnabled(dir), true));
	project((dir) => assert.equal(cloudDefaultEnabled(dir), true), JSON.stringify({ [CLOUD_DEFAULT_KEY]: true }));
});

test("CA-CLOUD-7: an unreadable settings file falls back to Cloud on", () => {
	project((dir) => assert.equal(cloudDefaultEnabled(dir), true), "{ not json");
});

test("CA-CLOUD-11: an untrusted project's cloudDefault is ignored", () => {
	project((dir) => assert.equal(cloudDefaultEnabled(dir, false), true), JSON.stringify({ [CLOUD_DEFAULT_KEY]: false }));
	project((dir) => {
		mkdirSync(join(dir, "agent"), { recursive: true });
		writeFileSync(join(dir, "agent", "settings.json"), JSON.stringify({ [CLOUD_DEFAULT_KEY]: false }));
		assert.equal(cloudDefaultEnabled(dir, false), false);
	}, JSON.stringify({ [CLOUD_DEFAULT_KEY]: true }));
});

test("CA-CLOUD-8: a recorded choice wins over the setting, and an option can still force Off", () => {
	assert.equal(resolveCloudEnabled(undefined, undefined, false), false);
	assert.equal(resolveCloudEnabled(undefined, undefined, true), true);
	assert.equal(resolveCloudEnabled(true, undefined, false), true);
	assert.equal(resolveCloudEnabled(false, undefined, true), false);
	assert.equal(resolveCloudEnabled(true, false, true), false);
});

test("CA-CLOUD-10: a delegated child inherits the parent's effective On when the default is off", () => {
	assert.equal(resolveCloudEnabled(undefined, true, false), true);
	assert.equal(resolveCloudEnabled(undefined, true, true), true);
});

test("CA-CLOUD-12: a malformed recorded choice is ignored, not read as On", () => {
	assert.equal(resolveCloudEnabled("false", undefined, false), false);
	assert.equal(resolveCloudEnabled("true", undefined, false), false);
	assert.equal(resolveCloudEnabled(null, undefined, false), false);
	assert.equal(resolveCloudEnabled(1, undefined, false), false);
	assert.equal(resolveCloudEnabled(undefined, true, false), true);
	assert.equal(resolveCloudEnabled("false", false, true), false);
});
