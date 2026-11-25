import { writeSync } from "node:fs";
import { InteractiveMode, type SessionManager } from "@earendil-works/pi-coding-agent";

import { formatResumeCommand } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/interactive-mode.js";

type SessionIdentity = Pick<SessionManager, "getSessionId">;

interface RecoveryHost {
	sessionManager: SessionManager;
	ui: { stop(options?: { preserveScreen?: boolean }): void };
}

interface RecoveryPrototype {
	registerSignalHandlers?(this: RecoveryHost): void;
	handleCtrlZ?(this: RecoveryHost): void;
}

export function announceRemoteSession(session: SessionIdentity): void {
	// Older remote clients can pass this private terminal sequence through unchanged.
	process.stdout.write(`\x1b]6973;${session.getSessionId()}\x07`);
}

function recoveryCommand(session: SessionManager): string | undefined {
	try {
		// Pi normally skips empty sessions. Save one here so its printed ID can be reopened.
		session.saveSession();
		return formatResumeCommand(session);
	} catch {
		return undefined;
	}
}

function showRecovery(session: SessionManager, paused: boolean, remote: boolean): void {
	const command = recoveryCommand(session);
	if (remote) return; // The client prints after restoring the user's terminal.
	const id = session.getSessionId();
	const line = paused
		? `Session ${id} paused. Return now with fg.${command ? ` Resume later with ${command}` : ""}`
		: command
			? `Session ${id} saved. Resume with ${command}`
			: `Session ${id} could not be saved. Start again with cloudthinker agent`;
	try {
		// Exit listeners run synchronously, and an SSH terminal may already be gone.
		writeSync(2, `${line}\n`);
	} catch {
		// The terminal is no longer writable.
	}
}

export function applySessionRecovery(remote = process.env.CLOUDTHINKER_REMOTE === "1"): void {
	const prototype = InteractiveMode.prototype as unknown as RecoveryPrototype;
	const { registerSignalHandlers, handleCtrlZ } = prototype;
	if (!registerSignalHandlers || !handleCtrlZ) {
		throw new Error("Pi's interactive lifecycle changed. Check session recovery after leaving the terminal.");
	}
	let active: RecoveryHost | undefined;
	prototype.registerSignalHandlers = function () {
		registerSignalHandlers.call(this);
		if (!active) process.once("exit", () => {
			if (active) showRecovery(active.sessionManager, false, remote);
		});
		active = this;
	};
	if (remote) return;
	prototype.handleCtrlZ = function () {
		const { ui } = this;
		const stop = ui.stop;
		ui.stop = (options) => {
			stop.call(ui, options);
			showRecovery(this.sessionManager, true, false);
		};
		try {
			handleCtrlZ.call(this);
		} finally {
			ui.stop = stop;
		}
	};
}
