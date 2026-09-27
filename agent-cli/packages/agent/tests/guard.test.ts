import assert from "node:assert/strict";
import test from "node:test";

import {
	COMMAND_ALIASES,
	DISABLED_COMMANDS,
	SESSION_REPLACEMENT,
	SHARE_STATUS,
	THINKING_STATUS,
	applyGuard,
	builtinSlashCommands,
	hasNoSessionFlag,
	interactiveModePrototype,
	removeDisabledCommands,
	registerCommandAliases,
	rewrittenCommand,
	wrapSubmitHandler,
} from "../src/guard.ts";

interface FakeHost {
	defaultEditor: { onSubmit?: (text: string) => unknown };
	editor: { setText: (text: string) => void };
	showStatus: (message: string) => void;
	statuses: string[];
	cleared: number;
	delivered: string[];
}

function fakeHost(): FakeHost {
	const host: FakeHost = {
		defaultEditor: {},
		editor: { setText: () => (host.cleared += 1) },
		showStatus: (message) => host.statuses.push(message),
		statuses: [],
		cleared: 0,
		delivered: [],
	};
	return host;
}

function fakePrototype(host: FakeHost): { setupEditorSubmitHandler?: () => void } {
	return {
		setupEditorSubmitHandler() {
			host.defaultEditor.onSubmit = (text: string) => {
				host.delivered.push(text);
				return "original";
			};
		},
	};
}

test("pi still exposes the two seams the guard patches", () => {
	assert.equal(typeof interactiveModePrototype().setupEditorSubmitHandler, "function");
	const offered = builtinSlashCommands().map((command) => command.name);
	for (const name of DISABLED_COMMANDS) {
		assert.ok(offered.includes(name), `pi no longer offers /${name}`);
	}
});

test("--no-session is seen before the separator and not after it", () => {
	assert.equal(hasNoSessionFlag(["--no-session"]), true);
	assert.equal(hasNoSessionFlag(["--model", "cloudthinker/pro", "--no-session"]), true);
	assert.equal(hasNoSessionFlag(["--", "--no-session"]), false);
	assert.equal(hasNoSessionFlag(["--resume"]), false);
	assert.equal(hasNoSessionFlag([]), false);
});

test("the wrapped handler swallows the disabled commands and delegates the rest", () => {
	const host = fakeHost();
	const prototype = fakePrototype(host);
	wrapSubmitHandler(prototype);
	prototype.setupEditorSubmitHandler?.call(host);
	const submit = host.defaultEditor.onSubmit;
	assert.ok(submit);

	assert.equal(submit("/share"), undefined);
	assert.equal(submit("  /thinking  "), undefined);
	assert.equal(submit("/thinking high"), undefined);
	assert.deepEqual(host.statuses, [SHARE_STATUS, THINKING_STATUS, THINKING_STATUS]);
	assert.equal(host.cleared, 3);
	assert.deepEqual(host.delivered, []);

	assert.equal(submit("/model"), "original");
	assert.equal(submit("/sharing is fine"), "original");
	assert.equal(submit("think about /share"), "original");
	assert.deepEqual(host.delivered, ["/model", "/sharing is fine", "think about /share"]);
	assert.equal(host.cleared, 3);
});

test("slash aliases rewrite only exact command input", () => {
	assert.deepEqual(
		COMMAND_ALIASES.map(({ alias, target }) => [alias, target]),
		[
			["exit", "quit"],
			["clear", "new"],
			["config", "settings"],
		],
	);
	assert.equal(rewrittenCommand(" /exit "), "/quit");
	assert.equal(rewrittenCommand("/clear"), "/new");
	assert.equal(rewrittenCommand("/config"), "/settings");
	assert.equal(rewrittenCommand("/exit now"), undefined);
	assert.equal(rewrittenCommand("please /clear"), undefined);
});

test("aliases dispatch through Pi's real built-in command handler", async () => {
	const actions: string[] = [];
	const host = Object.assign(fakeHost(), {
		showSettingsSelector: () => actions.push("settings"),
		handleClearCommand: async () => actions.push("new"),
		shutdown: async () => actions.push("quit"),
	});
	const prototype = { setupEditorSubmitHandler: interactiveModePrototype().setupEditorSubmitHandler };
	wrapSubmitHandler(prototype);
	prototype.setupEditorSubmitHandler?.call(host);
	const submit = host.defaultEditor.onSubmit;
	assert.ok(submit);

	await submit("/config");
	await submit("/clear");
	await submit("/exit");

	assert.deepEqual(actions, ["settings", "new", "quit"]);
	assert.deepEqual(host.delivered, []);
	assert.equal(host.cleared, 6);
});

test("a missing submit handler fails loudly instead of silently leaving the commands live", () => {
	assert.throws(() => wrapSubmitHandler({}), /setupEditorSubmitHandler/);
});

test("only the disabled names are spliced out of a command list", () => {
	const commands = [
		{ name: "model" },
		{ name: "thinking" },
		{ name: "share" },
		{ name: "session" },
	];
	assert.deepEqual(removeDisabledCommands(commands).sort(), ["share", "thinking"]);
	assert.deepEqual(
		commands.map((command) => command.name),
		["model", "session"],
	);
});

test("command aliases appear in Pi's slash-command picker", () => {
	const commands = ["quit", "new", "settings"].map((name) => ({ name }));
	registerCommandAliases(commands);
	assert.deepEqual(
		commands.map(({ name }) => name),
		["quit", "new", "settings", "exit", "clear", "config"],
	);
	assert.throws(() => registerCommandAliases([{ name: "quit" }]), /no longer offers \/new/);
});

test("the guard takes the two commands off the installed pi and then refuses to run twice", () => {
	applyGuard();
	const offered = builtinSlashCommands().map((command) => command.name);
	for (const name of DISABLED_COMMANDS) {
		assert.equal(offered.includes(name), false);
	}
	assert.ok(offered.includes("model"));
	for (const { alias } of COMMAND_ALIASES) assert.ok(offered.includes(alias));
	assert.throws(() => applyGuard(), /BUILTIN_SLASH_COMMANDS/);
});

test("/session renders the CloudThinker panel instead of pi's priced one", () => {
	const host = fakeHost();
	const prototype = fakePrototype(host);
	wrapSubmitHandler(prototype);
	prototype.setupEditorSubmitHandler?.call(host);
	const submit = host.defaultEditor.onSubmit;
	assert.ok(submit);

	submit(" /session ");
	submit("/sessions of work");

	assert.deepEqual(host.delivered, [SESSION_REPLACEMENT, "/sessions of work"]);
	assert.equal(host.cleared, 1);
});
