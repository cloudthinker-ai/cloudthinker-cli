import assert from "node:assert/strict";
import test from "node:test";

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { type Terminal, TuiAltScreen } from "@earendil-works/pi-tui";

import {
	FOCUS_REPORTING_OFF,
	FOCUS_REPORTING_ON,
	NOTIFY_MAX_CHARS,
	focusEvent,
	notificationSequence,
	setNotificationsEnabled,
	setTerminalFocus,
	terminalFocus,
	terminalNotify,
} from "@cloudthinker/cloud/src/notify.ts";

import { applyFocusTracking, registerNotifications } from "../src/notify.ts";

const IN = "\x1b[I";
const OUT = "\x1b[O";

function capture(isTTY: boolean): { isTTY: boolean; written: string[]; write(data: string): void } {
	const output = { isTTY, written: [] as string[], write: (data: string) => { output.written.push(data); } };
	return output;
}

function reset(): void {
	setTerminalFocus("unknown");
	setNotificationsEnabled(true);
}

test("focus reports move the state between focused and unfocused and ignore other input", () => {
	reset();
	assert.equal(terminalFocus(), "unknown");
	assert.equal(focusEvent(OUT), "unfocused");
	assert.equal(focusEvent(IN), "focused");
	assert.equal(focusEvent("a"), undefined);
	assert.equal(focusEvent("\x1b[A"), undefined);
});

test("the escape sequence follows the terminal: OSC 9, OSC 99 on kitty, OSC 777 on foot and rxvt, each after a BEL", () => {
	assert.equal(notificationSequence("done", { TERM: "xterm-256color", TERM_PROGRAM: "iTerm.app" }), "\x07\x1b]9;done\x07");
	assert.equal(notificationSequence("done", { TERM: "xterm-ghostty", TERM_PROGRAM: "ghostty" }), "\x07\x1b]9;done\x07");
	assert.equal(notificationSequence("done", { TERM: "xterm-256color", TERM_PROGRAM: "WezTerm" }), "\x07\x1b]9;done\x07");
	assert.equal(notificationSequence("done", { TERM: "xterm-kitty" }), "\x07\x1b]99;;done\x1b\\");
	assert.equal(notificationSequence("done", { TERM: "xterm-256color", KITTY_WINDOW_ID: "1" }), "\x07\x1b]99;;done\x1b\\");
	assert.equal(notificationSequence("a;b", { TERM: "foot" }), "\x07\x1b]777;notify;CloudThinker;a,b\x07");
	assert.equal(notificationSequence("done", { TERM: "rxvt-unicode-256color" }), "\x07\x1b]777;notify;CloudThinker;done\x07");
});

test("under tmux the OSC is wrapped for passthrough and the BEL stays plain", () => {
	assert.equal(notificationSequence("done", { TERM: "tmux-256color", TMUX: "/tmp/tmux-1/default,1,0" }), "\x07\x1bPtmux;\x1b\x1b]9;done\x07\x1b\\");
	assert.equal(notificationSequence("done", { TERM: "xterm-kitty", TMUX: "/tmp/tmux-1/default,1,0" }), "\x07\x1bPtmux;\x1b\x1b]99;;done\x1b\x1b\\\x1b\\");
});

test("a notification strips control characters and caps the text", () => {
	assert.equal(notificationSequence("a\x1b]52;c;evil\x07b\nc", {}), "\x07\x1b]9;a ]52;c;evil b c\x07");
	assert.equal(notificationSequence("x".repeat(NOTIFY_MAX_CHARS + 50), {}), `\x07\x1b]9;${"x".repeat(NOTIFY_MAX_CHARS)}\x07`);
});

test("a notification is written only to a terminal, only while the window is known to be unfocused, and only while the setting is on", () => {
	reset();
	const tty = capture(true);
	terminalNotify("done", {}, tty);
	assert.deepEqual(tty.written, [], "focus unknown sends nothing");
	setTerminalFocus("focused");
	terminalNotify("done", {}, tty);
	assert.deepEqual(tty.written, [], "focused sends nothing");
	setTerminalFocus("unfocused");
	terminalNotify("done", {}, tty);
	assert.deepEqual(tty.written, ["\x07\x1b]9;done\x07"]);
	const pipe = capture(false);
	terminalNotify("done", {}, pipe);
	assert.deepEqual(pipe.written, []);
	const envOff = capture(true);
	terminalNotify("done", { CLOUDTHINKER_NOTIFY: "0" }, envOff);
	assert.deepEqual(envOff.written, []);
	setNotificationsEnabled(false);
	const settingOff = capture(true);
	terminalNotify("done", {}, settingOff);
	assert.deepEqual(settingOff.written, []);
	reset();
});

type Handler = (event: unknown, ctx: unknown) => unknown;

function harness(isTTY: boolean) {
	const handlers = new Map<string, Handler>();
	const pi = { on: (event: string, handler: Handler) => { handlers.set(event, handler); } } as unknown as Pick<ExtensionAPI, "on">;
	let clock = 0;
	const sent: string[] = [];
	const output = capture(isTTY);
	registerNotifications(pi, () => clock, (text) => sent.push(text), output);
	const listeners: ((data: string) => unknown)[] = [];
	const ui = { onTerminalInput: (listener: (data: string) => unknown) => { listeners.push(listener); return () => { listeners.splice(listeners.indexOf(listener), 1); }; } };
	const tui = { mode: "tui", hasUI: true, ui, cwd: process.cwd(), isProjectTrusted: () => false };
	return {
		output,
		sent,
		listeners,
		tui,
		advance: (ms: number) => { clock += ms; },
		emit: (name: string, ctx: unknown) => handlers.get(name)?.({ type: name, messages: [] }, ctx),
	};
}

test("a TUI session turns focus reporting on, tracks focus-in and focus-out through the input layer, and turns it off at shutdown", () => {
	reset();
	const run = harness(true);
	run.emit("session_start", run.tui);
	assert.deepEqual(run.output.written, [FOCUS_REPORTING_ON]);
	assert.equal(run.listeners.length, 1);
	assert.deepEqual(run.listeners[0]?.(OUT), { consume: true });
	assert.equal(terminalFocus(), "unfocused");
	assert.deepEqual(run.listeners[0]?.(IN), { consume: true });
	assert.equal(terminalFocus(), "focused");
	assert.equal(run.listeners[0]?.("x"), undefined);
	run.emit("session_shutdown", run.tui);
	assert.deepEqual(run.output.written, [FOCUS_REPORTING_ON, FOCUS_REPORTING_OFF]);
	assert.equal(run.listeners.length, 0);
	reset();
});

test("a print or json run never tracks focus and a turn end there never notifies", () => {
	reset();
	const run = harness(true);
	const headless = { mode: "print", hasUI: false, ui: run.tui.ui, cwd: process.cwd(), isProjectTrusted: () => false };
	run.emit("session_start", headless);
	run.emit("agent_start", headless);
	run.emit("agent_end", headless);
	assert.deepEqual(run.output.written, []);
	assert.equal(run.listeners.length, 0);
	assert.deepEqual(run.sent, []);
});

test("a TUI turn end notifies once, a stray end does not, and a non-terminal output tracks nothing", () => {
	reset();
	const run = harness(true);
	run.emit("agent_start", run.tui);
	run.advance(12_400);
	run.emit("agent_end", run.tui);
	run.emit("agent_end", run.tui);
	assert.deepEqual(run.sent, ["CloudThinker: done after 12s"]);
	const piped = harness(false);
	piped.emit("session_start", piped.tui);
	assert.deepEqual(piped.output.written, []);
	assert.equal(piped.listeners.length, 0);
	reset();
});

test("in fullscreen a focus report typed into pi-tui's terminal reaches the notifier, so a turn that ends while the window is away notifies", () => {
	reset();
	applyFocusTracking();
	let type: (data: string) => void = () => {};
	const terminal: Terminal = {
		start(onInput) { type = onInput; }, stop() {}, drainInput: async () => {}, write() {},
		columns: 40, rows: 10, kittyProtocolActive: false,
		moveBy() {}, hideCursor() {}, showCursor() {}, clearLine() {}, clearFromCursor() {}, clearScreen() {}, setTitle() {}, setProgress() {},
	};
	const screen = new TuiAltScreen(terminal, false, undefined, { mouse: true });
	const handlers = new Map<string, Handler>();
	const pi = { on: (event: string, handler: Handler) => { handlers.set(event, handler); } } as unknown as Pick<ExtensionAPI, "on">;
	const tty = capture(true);
	registerNotifications(pi, () => 0, (text) => terminalNotify(text, { TERM: "xterm-256color", TERM_PROGRAM: "ghostty" }, tty), tty);
	const ctx = { mode: "tui", hasUI: true, ui: { onTerminalInput: (listener: (data: string) => { consume?: boolean } | undefined) => screen.addInputListener(listener) }, cwd: process.cwd(), isProjectTrusted: () => false };
	const emit = (name: string) => handlers.get(name)?.({ type: name, messages: [] }, ctx);
	screen.start();
	try {
		emit("session_start");
		type(OUT);
		assert.equal(terminalFocus(), "unfocused");
		emit("agent_start");
		emit("agent_end");
		assert.deepEqual(tty.written, [FOCUS_REPORTING_ON, "\x07\x1b]9;CloudThinker: done after 0s\x07"]);
		type(IN);
		assert.equal(terminalFocus(), "focused");
		emit("agent_start");
		emit("agent_end");
		assert.equal(tty.written.length, 2);
	} finally {
		emit("session_shutdown");
		screen.stop();
		reset();
	}
});
