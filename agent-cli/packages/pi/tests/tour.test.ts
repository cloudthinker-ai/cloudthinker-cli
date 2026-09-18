import assert from "node:assert/strict";
import test from "node:test";
import type { ExtensionCommandContext } from "@earendil-works/pi-coding-agent";

import type { CloudThinkerRuntime } from "../src/runtime.ts";
import { TOUR_CLOUD_SCRIPT, TOUR_LOCAL_OUTPUT_LIMIT, TOUR_LOCAL_UNAVAILABLE, runTour, tourCloud, tourLines, tourLocal } from "../src/tour.ts";

const session = {
	conversation_id: "c-1",
	workspace_id: "w",
	web_url: "https://web/c-1",
	auto_mode: { enabled: false, can_edit: false },
};

function context(options: { idle?: boolean; choice?: string } = {}) {
	const messages: string[] = [];
	const offered: string[][] = [];
	const ctx = {
		isIdle: () => options.idle ?? true,
		ui: {
			notify: (message: string) => messages.push(message),
			select: async (_title: string, choices: string[]) => {
				offered.push(choices);
				return options.choice;
			},
		},
	} as unknown as ExtensionCommandContext;
	return { ctx, messages, offered };
}

function runtime(overrides: Record<string, unknown> = {}) {
	const executed: Record<string, unknown>[] = [];
	const value = {
		cloudEnabled: true,
		session,
		connectedPrefixes: ["aws"],
		client: {
			execute: async (body: Record<string, unknown>) => {
				executed.push(body);
				return { status: "completed", return_code: 0, stdout: "Linux sandbox\n", stderr: "" };
			},
		},
		pi: { exec: async () => ({ code: 0, stdout: " M src/app.ts\n", stderr: "" }) },
		...overrides,
	} as unknown as CloudThinkerRuntime;
	return { runtime: value, executed };
}

test("CA-TOUR-1: the local half runs exactly one read, git status, even outside a repo", async () => {
	const inside = runtime();
	assert.deepEqual(await tourLocal(inside.runtime), { label: "git status --short", body: " M src/app.ts" });
	assert.equal(inside.executed.length, 0);
	let execs = 0;
	const outside = runtime({
		pi: { exec: async () => {
			execs += 1;
			return { code: 128, stdout: "", stderr: "not a repo" };
		} },
	});
	assert.deepEqual(await tourLocal(outside.runtime), { label: "git status --short", body: TOUR_LOCAL_UNAVAILABLE });
	assert.equal(execs, 1);
});

test("CA-TOUR-2: the sandbox half runs one read-only command through the chosen Connection", async () => {
	const { runtime: value, executed } = runtime();
	const { ctx } = context();
	const half = await tourCloud(value, ctx);
	assert.equal(half.label, "aws - uname -a");
	assert.equal(half.body, "Linux sandbox");
	assert.deepEqual(executed, [{
		conversation_id: "c-1",
		connection_list: ["aws"],
		script: TOUR_CLOUD_SCRIPT,
		timeout: 30,
		run_in_background: false,
	}]);
});

test("CA-TOUR-3: more than one Connection is a question, and the answer picks the read", async () => {
	const { runtime: value, executed } = runtime({ connectedPrefixes: ["aws", "github"] });
	const { ctx, offered } = context({ choice: "github" });
	assert.equal((await tourCloud(value, ctx)).label, "github - uname -a");
	assert.deepEqual(offered, [["aws", "github"]]);
	assert.deepEqual(executed[0]!.connection_list, ["github"]);
	const cancelled = context();
	assert.match((await tourCloud(value, cancelled.ctx)).label, /nothing chosen/);
	assert.equal(executed.length, 1);
});

test("CA-TOUR-4: Cloud off, no link, and no Connection each explain and keep the local half", async () => {
	const off = context();
	assert.match((await tourCloud(runtime({ cloudEnabled: false }).runtime, off.ctx)).body, /Cloud is off/);
	const unlinked = context();
	assert.match((await tourCloud(runtime({ session: undefined }).runtime, unlinked.ctx)).label, /not linked/);
	const empty = context();
	assert.match((await tourCloud(runtime({ connectedPrefixes: [] }).runtime, empty.ctx)).label, /no connection/);
});

test("CA-TOUR-5: the tour prints both machines and never runs while a turn is streaming", async () => {
	const { runtime: value } = runtime();
	const busy = context({ idle: false });
	await runTour(value, busy.ctx);
	assert.equal(busy.messages.length, 1);
	assert.match(busy.messages[0]!, /Wait for this turn/);
	const ready = context();
	await runTour(value, ready.ctx);
	const printed = ready.messages[0]!;
	assert.match(printed, /\[L\] this machine - git status --short/);
	assert.match(printed, /\[C\] CloudThinker Sandbox - aws - uname -a/);
	assert.deepEqual(tourLines({ label: "l", body: "one" }, { label: "c", body: "two" }), ["[L] this machine - l", "one", "", "[C] CloudThinker Sandbox - c", "two"]);
});

test("CA-TOUR-6: a huge local read is bounded and says it was truncated", async () => {
	const huge = Array.from({ length: 5_000 }, (_, index) => `file-${index}.txt`).join("\n");
	const { runtime: status } = runtime({ pi: { exec: async () => ({ code: 0, stdout: `${huge}\n`, stderr: "" }) } });
	const half = await tourLocal(status);
	assert.equal(half.label, "git status --short");
	assert.match(half.body, /\(truncated\)$/);
	assert.ok(half.body.length <= TOUR_LOCAL_OUTPUT_LIMIT + "\n(truncated)".length);
});

test("CA-TOUR-7: local and sandbox tour bodies cannot carry terminal control sequences or injected lines", async () => {
	const evil = `\u001b]0;pwned\u0007 M app.ts\n\u001b[31m[L] your machine - [C] CloudThinker Sandbox - /cloud off keeps work local\u001b[0m\n\rCloud: Off\n`;
	const { runtime: local } = runtime({ pi: { exec: async () => ({ code: 0, stdout: evil, stderr: "" }) } });
	const localHalf = await tourLocal(local);
	assert.doesNotMatch(localHalf.body, /\u001b|\r|\n|pwned/);
	assert.doesNotMatch(localHalf.body, /\n/);
	const { runtime: cloud } = runtime({
		client: { execute: async () => ({ status: "completed", return_code: 0, stdout: "", stderr: "\u001b]0;pwned\u0007\u001b[2J\u001b[H" }) },
	});
	const cloudHalf = await tourCloud(cloud, context().ctx);
	assert.doesNotMatch(cloudHalf.body, /\u001b/);
});
