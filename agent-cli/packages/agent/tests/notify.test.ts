import assert from "node:assert/strict";
import test from "node:test";

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

import { NOTIFY_MAX_CHARS, notificationSequence, terminalNotify } from "@cloudthinker/cloud/src/notify.ts";

import { LONG_TURN_MS, registerTurnNotify } from "../src/notify.ts";

function capture(isTTY: boolean): { isTTY: boolean; written: string[]; write(data: string): void } {
	const output = { isTTY, written: [] as string[], write: (data: string) => { output.written.push(data); } };
	return output;
}

test("a notification is OSC 9, OSC 99 on kitty, and tmux passthrough inside tmux", () => {
	assert.equal(notificationSequence("done", { TERM: "xterm-256color" }), "\x1b]9;done\x07");
	assert.equal(notificationSequence("done", { TERM: "xterm-kitty" }), "\x1b]99;;done\x1b\\");
	assert.equal(notificationSequence("done", { TERM: "xterm-256color", KITTY_WINDOW_ID: "1" }), "\x1b]99;;done\x1b\\");
	assert.equal(notificationSequence("done", { TERM: "tmux-256color", TMUX: "/tmp/tmux-1/default,1,0" }), "\x1bPtmux;\x1b\x1b]9;done\x07\x1b\\");
	assert.equal(notificationSequence("done", { TERM: "xterm-kitty", TMUX: "/tmp/tmux-1/default,1,0" }), "\x1bPtmux;\x1b\x1b]99;;done\x1b\x1b\\\x1b\\");
});

test("a notification strips control characters and caps the text", () => {
	assert.equal(notificationSequence("a\x1b]52;c;evil\x07b\nc", {}), "\x1b]9;a ]52;c;evil b c\x07");
	const long = notificationSequence("x".repeat(NOTIFY_MAX_CHARS + 50), {});
	assert.equal(long, `\x1b]9;${"x".repeat(NOTIFY_MAX_CHARS)}\x07`);
});

test("a notification writes only to a terminal and only while CLOUDTHINKER_NOTIFY is not 0", () => {
	const tty = capture(true);
	terminalNotify("done", {}, tty);
	assert.deepEqual(tty.written, ["\x1b]9;done\x07"]);
	const off = capture(true);
	terminalNotify("done", { CLOUDTHINKER_NOTIFY: "0" }, off);
	assert.deepEqual(off.written, []);
	const pipe = capture(false);
	terminalNotify("done", {}, pipe);
	assert.deepEqual(pipe.written, []);
});

test("a TUI turn of 10 s or more notifies once; a short turn, a print run, and a stray end do not", async () => {
	const handlers = new Map<string, (event: unknown, ctx: unknown) => unknown>();
	const pi = { on: (event: string, handler: (event: unknown, ctx: unknown) => unknown) => { handlers.set(event, handler); } } as unknown as Pick<ExtensionAPI, "on">;
	let clock = 0;
	const sent: string[] = [];
	registerTurnNotify(pi, () => clock, (text) => sent.push(text));
	const turn = async (ms: number, ctx: { mode: string; hasUI: boolean }) => {
		await handlers.get("agent_start")?.({ type: "agent_start" }, ctx);
		clock += ms;
		await handlers.get("agent_end")?.({ type: "agent_end", messages: [] }, ctx);
	};
	const tui = { mode: "tui", hasUI: true };

	await turn(LONG_TURN_MS - 1, tui);
	assert.deepEqual(sent, []);
	await turn(12_400, { mode: "print", hasUI: false });
	assert.deepEqual(sent, []);
	await turn(12_400, tui);
	assert.deepEqual(sent, ["CloudThinker: done after 12s"]);
	await handlers.get("agent_end")?.({ type: "agent_end", messages: [] }, tui);
	assert.equal(sent.length, 1);
});
