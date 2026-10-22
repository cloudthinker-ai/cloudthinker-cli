import { hostname } from "node:os";

import { InteractiveMode } from "@earendil-works/pi-coding-agent";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

import type { RemoteEnv } from "./remote-attach.ts";

export const REMOTE_DETACH_SIGNAL = "SIGUSR1";
export const DETACH_COMMAND = "detach";
export const DETACH_DESCRIPTION = "Detach; the agent keeps running and cloudthinker remote returns to it";
export const DETACH_CHOICE = "Detach   it keeps running; come back any time";
export const QUIT_CHOICE = "Quit     stops the agent and its running turn";
export const NO_HOST_NOTICE = "Can't detach: this agent has no cloudthinker remote host · /quit stops it";
export const HOST_GONE_NOTICE = "Can't detach: the cloudthinker remote host has gone · /quit stops the agent";

export type SignalHost = (pid: number, signal: string) => void;

export interface ShutdownOptions {
	fromSignal?: boolean;
}

export interface LeaveHost {
	isShuttingDown?: boolean;
	extensionSelector?: unknown;
	showExtensionSelector(title: string, options: string[]): Promise<string | undefined>;
	showStatus(message: string): void;
}

export interface LeavePrototype {
	shutdown?: (this: LeaveHost, options?: ShutdownOptions) => Promise<void>;
}

export interface SuspendPrototype {
	handleCtrlZ?: (this: Pick<LeaveHost, "showStatus">) => void;
}

const signalProcess: SignalHost = (pid, signal) => {
	process.kill(pid, signal);
};

export function leaveTitle(host: string = hostname()): string {
	return `Leave the agent on ${host}?`;
}

export function detachFromHost(env: RemoteEnv = process.env, kill: SignalHost = signalProcess): string | undefined {
	const pid = Number(env.CLOUDTHINKER_REMOTE_HOST_PID);
	if (!Number.isSafeInteger(pid) || pid <= 1) return NO_HOST_NOTICE;
	try {
		kill(pid, REMOTE_DETACH_SIGNAL);
	} catch {
		return HOST_GONE_NOTICE;
	}
	return undefined;
}

export function wrapRemoteLeave(
	prototype: LeavePrototype,
	env: RemoteEnv = process.env,
	kill: SignalHost = signalProcess,
	host: string = hostname(),
): void {
	const shutdown = prototype.shutdown;
	if (typeof shutdown !== "function") {
		throw new Error("pi's InteractiveMode no longer defines shutdown, so Ctrl+D, Ctrl+C twice, /quit, and /exit under cloudthinker remote would quit without offering detach");
	}
	const open = new WeakMap<LeaveHost, unknown>();
	prototype.shutdown = async function (options) {
		if (options?.fromSignal || this.isShuttingDown) return shutdown.call(this, options);
		if (open.has(this) && open.get(this) === this.extensionSelector) return;
		const asked = this.showExtensionSelector(leaveTitle(host), [DETACH_CHOICE, QUIT_CHOICE]);
		const dialog = this.extensionSelector ?? Symbol("leave dialog");
		open.set(this, dialog);
		let choice: string | undefined;
		try {
			choice = await asked;
		} finally {
			if (open.get(this) === dialog) open.delete(this);
		}
		if (choice === QUIT_CHOICE) return shutdown.call(this, options);
		if (choice !== DETACH_CHOICE) return;
		const notice = detachFromHost(env, kill);
		if (notice) this.showStatus(notice);
	};
}

export function wrapRemoteSuspend(
	prototype: SuspendPrototype,
	env: RemoteEnv = process.env,
	kill: SignalHost = signalProcess,
): void {
	if (typeof prototype.handleCtrlZ !== "function") {
		throw new Error("pi's InteractiveMode no longer defines handleCtrlZ, so Ctrl+Z under cloudthinker remote would freeze the session instead of detaching");
	}
	prototype.handleCtrlZ = function () {
		const notice = detachFromHost(env, kill);
		if (notice) this.showStatus(notice);
	};
}

export function registerRemoteDetach(
	pi: Pick<ExtensionAPI, "registerCommand">,
	env: RemoteEnv = process.env,
	kill: SignalHost = signalProcess,
): void {
	if (env.CLOUDTHINKER_REMOTE !== "1") return;
	pi.registerCommand(DETACH_COMMAND, {
		description: DETACH_DESCRIPTION,
		handler: async (_args, ctx) => {
			const notice = detachFromHost(env, kill);
			if (notice) ctx.ui.notify(notice, "warning");
		},
	});
}

export function applyRemoteLeave(env: RemoteEnv = process.env): void {
	if (env.CLOUDTHINKER_REMOTE !== "1") return;
	wrapRemoteLeave(InteractiveMode.prototype as unknown as LeavePrototype, env);
	wrapRemoteSuspend(InteractiveMode.prototype as unknown as SuspendPrototype, env);
}
