import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { InteractiveMode } from "@earendil-works/pi-coding-agent";

import {
	REDRAW_COALESCE_MS,
	REMOTE_ATTACH_SIGNAL,
	type RemoteAttachPrototype,
	type RemoteAttachUi,
	applyRemoteAttach,
	wrapRemoteAttach,
} from "../src/remote-attach.ts";

test("outside cloudthinker remote the hook leaves pi alone and installs no signal handler", () => {
	const prototype = InteractiveMode.prototype as unknown as RemoteAttachPrototype;
	const original = prototype.setupKeyHandlers;
	const listeners = process.listenerCount(REMOTE_ATTACH_SIGNAL);
	applyRemoteAttach({});
	applyRemoteAttach({ CLOUDTHINKER_REMOTE: "0" });
	assert.equal(prototype.setupKeyHandlers, original);
	assert.equal(process.listenerCount(REMOTE_ATTACH_SIGNAL), listeners);
	assert.throws(() => wrapRemoteAttach({}, { CLOUDTHINKER_REMOTE: "1" }), /setupKeyHandlers/);
});

test("a reattach signal redraws the live screen once, after the handler wrote the ready file", (t) => {
	t.mock.timers.enable({ apis: ["setTimeout"] });
	const dir = mkdtempSync(join(tmpdir(), "ct-remote-attach-"));
	const ready = join(dir, "default.ready");
	const before = new Set(process.listeners(REMOTE_ATTACH_SIGNAL));
	try {
		const calls: string[] = [];
		const ui: RemoteAttachUi = {
			stop: (options) => { calls.push(`stop:${JSON.stringify(options)}`); },
			start: () => { calls.push("start"); },
			requestRender: (force) => { calls.push(`render:${String(force)}`); },
		};
		let setups = 0;
		const prototype: RemoteAttachPrototype = { setupKeyHandlers() { setups += 1; } };
		wrapRemoteAttach(prototype, { CLOUDTHINKER_REMOTE: "1", CLOUDTHINKER_REMOTE_READY: ready });
		prototype.setupKeyHandlers?.call({ ui });
		prototype.setupKeyHandlers?.call({ ui });
		assert.equal(setups, 2);
		assert.equal(process.listenerCount(REMOTE_ATTACH_SIGNAL), before.size + 1);
		assert.equal(readFileSync(ready, "utf8"), String(process.pid));
		assert.equal(statSync(ready).mode & 0o777, 0o600);

		process.emit(REMOTE_ATTACH_SIGNAL);
		process.emit(REMOTE_ATTACH_SIGNAL);
		assert.deepEqual(calls, []);
		t.mock.timers.tick(REDRAW_COALESCE_MS);
		assert.deepEqual(calls, ['stop:{"preserveScreen":true}', "start", "render:true"]);

		ui.stopped = true;
		process.emit(REMOTE_ATTACH_SIGNAL);
		t.mock.timers.tick(REDRAW_COALESCE_MS);
		assert.equal(calls.length, 3);
	} finally {
		for (const listener of process.listeners(REMOTE_ATTACH_SIGNAL)) {
			if (!before.has(listener)) process.removeListener(REMOTE_ATTACH_SIGNAL, listener);
		}
		rmSync(dir, { recursive: true, force: true });
	}
});
