import assert from "node:assert/strict";
import test from "node:test";

import { InteractiveMode } from "@earendil-works/pi-coding-agent";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

import {
	DETACH_CHOICE,
	DETACH_COMMAND,
	HOST_GONE_NOTICE,
	NO_HOST_NOTICE,
	QUIT_CHOICE,
	REMOTE_DETACH_SIGNAL,
	type LeaveHost,
	type LeavePrototype,
	type ShutdownOptions,
	type SuspendPrototype,
	applyRemoteLeave,
	detachFromHost,
	registerRemoteDetach,
	wrapRemoteLeave,
	wrapRemoteSuspend,
} from "../src/remote-leave.ts";

const REMOTE = { CLOUDTHINKER_REMOTE: "1", CLOUDTHINKER_REMOTE_HOST_PID: "4242" };

type Command = { description?: string; handler: (args: string, ctx: never) => Promise<void> };

function commandsFor(env: Record<string, string>, kill: (pid: number, signal: string) => void): Map<string, Command> {
	const commands = new Map<string, Command>();
	const pi = { registerCommand: (name: string, command: Command) => { commands.set(name, command); } };
	registerRemoteDetach(pi as unknown as Pick<ExtensionAPI, "registerCommand">, env, kill);
	return commands;
}

function leaveHost(answer: string | undefined, isShuttingDown = false) {
	const shown: { title: string; options: string[] }[] = [];
	const statuses: string[] = [];
	const host: LeaveHost = {
		isShuttingDown,
		showExtensionSelector: async (title, options) => {
			shown.push({ title, options });
			return answer;
		},
		showStatus: (message) => { statuses.push(message); },
	};
	return { host, shown, statuses };
}

test("outside cloudthinker remote pi's shutdown stays as it is and /detach does not exist", () => {
	const prototype = InteractiveMode.prototype as unknown as LeavePrototype;
	const original = prototype.shutdown;
	const suspend = (InteractiveMode.prototype as unknown as SuspendPrototype).handleCtrlZ;
	applyRemoteLeave({});
	applyRemoteLeave({ CLOUDTHINKER_REMOTE: "0" });
	assert.equal(prototype.shutdown, original);
	assert.equal((InteractiveMode.prototype as unknown as SuspendPrototype).handleCtrlZ, suspend);
	assert.equal(commandsFor({}, () => {}).size, 0);
	assert.throws(() => wrapRemoteLeave({}, REMOTE), /shutdown/);
	assert.throws(() => wrapRemoteSuspend({}, REMOTE), /handleCtrlZ/);
});

test("/detach signals the remote host, and a missing or gone host gives one notice instead of a throw", async () => {
	const signals: [number, string][] = [];
	const notices: string[] = [];
	const ctx = { ui: { notify: (message: string) => { notices.push(message); } } } as never;
	const detach = commandsFor(REMOTE, (pid, signal) => { signals.push([pid, signal]); }).get(DETACH_COMMAND);
	assert.ok(detach);
	assert.ok(detach.description);
	await detach.handler("", ctx);
	assert.deepEqual(signals, [[4242, REMOTE_DETACH_SIGNAL]]);
	assert.equal(REMOTE_DETACH_SIGNAL, "SIGUSR1");
	assert.deepEqual(notices, []);

	for (const pid of [undefined, "", "abc", "0", "1", "-5", "12.5"]) {
		assert.equal(detachFromHost({ CLOUDTHINKER_REMOTE: "1", CLOUDTHINKER_REMOTE_HOST_PID: pid }, () => { signals.push([0, "unexpected"]); }), NO_HOST_NOTICE);
	}
	assert.equal(signals.length, 1);

	const gone = commandsFor(REMOTE, () => { throw new Error("kill ESRCH"); }).get(DETACH_COMMAND);
	await gone?.handler("", ctx);
	assert.deepEqual(notices, [HOST_GONE_NOTICE]);
});

test("the leave dialog detaches on its default choice, quits through pi's shutdown, and Esc returns to the agent", async () => {
	const signals: number[] = [];
	const exits: (ShutdownOptions | undefined)[] = [];
	const prototype: LeavePrototype = { shutdown: async (options) => { exits.push(options); } };
	wrapRemoteLeave(prototype, REMOTE, (pid) => { signals.push(pid); }, "product-team-4");

	const detach = leaveHost(DETACH_CHOICE);
	await prototype.shutdown?.call(detach.host);
	assert.deepEqual(detach.shown, [{ title: "Leave the agent on product-team-4?", options: [DETACH_CHOICE, QUIT_CHOICE] }]);
	assert.deepEqual(signals, [4242]);
	assert.deepEqual(exits, []);
	assert.deepEqual(detach.statuses, []);

	const esc = leaveHost(undefined);
	await prototype.shutdown?.call(esc.host);
	assert.equal(esc.shown.length, 1);
	assert.deepEqual(signals, [4242]);
	assert.deepEqual(exits, []);

	const quit = leaveHost(QUIT_CHOICE);
	await prototype.shutdown?.call(quit.host);
	assert.deepEqual(signals, [4242]);
	assert.deepEqual(exits, [undefined]);

	const signal = leaveHost(DETACH_CHOICE);
	await prototype.shutdown?.call(signal.host, { fromSignal: true });
	const ending = leaveHost(DETACH_CHOICE, true);
	await prototype.shutdown?.call(ending.host);
	assert.deepEqual(signal.shown, []);
	assert.deepEqual(ending.shown, []);
	assert.deepEqual(exits, [undefined, { fromSignal: true }, undefined]);
	assert.deepEqual(signals, [4242]);
});

function piLikeHost() {
	const answers: ((choice: string | undefined) => void)[] = [];
	const statuses: string[] = [];
	const host: LeaveHost = {
		showExtensionSelector(_title, _options) {
			const selector = {};
			this.extensionSelector = selector;
			return new Promise((resolve) => {
				answers.push((choice) => {
					if (this.extensionSelector === selector) this.extensionSelector = undefined;
					resolve(choice);
				});
			});
		},
		showStatus: (message) => { statuses.push(message); },
	};
	return { host, answers, statuses };
}

test("a second exit key while the dialog is open opens no second dialog, and a detach without a host says why", async () => {
	const { host, answers, statuses } = piLikeHost();
	const prototype: LeavePrototype = { shutdown: async () => {} };
	wrapRemoteLeave(prototype, { CLOUDTHINKER_REMOTE: "1" }, () => {}, "vm");
	const first = prototype.shutdown?.call(host);
	await prototype.shutdown?.call(host);
	assert.equal(answers.length, 1);
	answers[0]?.(DETACH_CHOICE);
	await first;
	assert.deepEqual(statuses, [NO_HOST_NOTICE]);
	const again = prototype.shutdown?.call(host);
	assert.equal(answers.length, 2);
	answers[1]?.(undefined);
	await again;
});

test("a leave dialog that a cloud-write approval replaced never blocks the next quit", async () => {
	const { host, answers } = piLikeHost();
	const exits: (ShutdownOptions | undefined)[] = [];
	const prototype: LeavePrototype = { shutdown: async (options) => { exits.push(options); } };
	wrapRemoteLeave(prototype, REMOTE, () => {}, "vm");
	void prototype.shutdown?.call(host);
	assert.equal(answers.length, 1);
	const approval = host.showExtensionSelector("Approve this write?", ["Approve", "Decline"]);
	answers[1]?.("Approve");
	assert.equal(await approval, "Approve");
	assert.equal(host.extensionSelector, undefined);

	const quitting = prototype.shutdown?.call(host);
	assert.equal(answers.length, 3);
	answers[2]?.(QUIT_CHOICE);
	await quitting;
	assert.deepEqual(exits, [undefined]);
});

test("a Ctrl+Z that reaches the agent under cloudthinker remote detaches instead of suspending, and says why when it cannot", () => {
	const signals: [number, string][] = [];
	const statuses: string[] = [];
	let suspended = 0;
	const host = { showStatus: (message: string) => { statuses.push(message); } };
	const prototype: SuspendPrototype = { handleCtrlZ() { suspended += 1; } };
	wrapRemoteSuspend(prototype, REMOTE, (pid, signal) => { signals.push([pid, signal]); });
	prototype.handleCtrlZ?.call(host);
	assert.deepEqual(signals, [[4242, REMOTE_DETACH_SIGNAL]]);
	assert.equal(suspended, 0);
	assert.deepEqual(statuses, []);

	const orphan: SuspendPrototype = { handleCtrlZ() { suspended += 1; } };
	wrapRemoteSuspend(orphan, { CLOUDTHINKER_REMOTE: "1" }, () => { signals.push([0, "unexpected"]); });
	orphan.handleCtrlZ?.call(host);
	assert.equal(suspended, 0);
	assert.equal(signals.length, 1);
	assert.deepEqual(statuses, [NO_HOST_NOTICE]);
});
