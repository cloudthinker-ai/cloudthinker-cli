import { renameSync, writeFileSync } from "node:fs";

import { InteractiveMode } from "@earendil-works/pi-coding-agent";

export const REMOTE_ATTACH_SIGNAL = "SIGUSR2";
export const REDRAW_COALESCE_MS = 100;

export type RemoteEnv = Readonly<Record<string, string | undefined>>;

export interface RemoteAttachUi {
	stopped?: boolean;
	stop(options?: { preserveScreen?: boolean }): void;
	start(): void;
	requestRender(force?: boolean): void;
}

export interface RemoteAttachPrototype {
	setupKeyHandlers?: (this: { ui: RemoteAttachUi }) => void;
}

export function redrawOnAttach(ui: RemoteAttachUi): () => void {
	let pending: ReturnType<typeof setTimeout> | undefined;
	return () => {
		if (pending !== undefined) return;
		pending = setTimeout(() => {
			pending = undefined;
			if (ui.stopped) return;
			try {
				ui.stop({ preserveScreen: true });
				ui.start();
				ui.requestRender(true);
			} catch {
				return;
			}
		}, REDRAW_COALESCE_MS);
	};
}

export function writeReadyFile(path: string, pid: number = process.pid): void {
	const staging = `${path}.${pid}.tmp`;
	writeFileSync(staging, String(pid), { mode: 0o600 });
	renameSync(staging, path);
}

export function wrapRemoteAttach(prototype: RemoteAttachPrototype, env: RemoteEnv = process.env): void {
	const setupKeyHandlers = prototype.setupKeyHandlers;
	if (typeof setupKeyHandlers !== "function") {
		throw new Error("pi's InteractiveMode no longer defines setupKeyHandlers, so cloudthinker remote cannot redraw the screen on reattach");
	}
	let installed = false;
	prototype.setupKeyHandlers = function () {
		setupKeyHandlers.call(this);
		if (installed) return;
		installed = true;
		process.on(REMOTE_ATTACH_SIGNAL, redrawOnAttach(this.ui));
		const ready = env.CLOUDTHINKER_REMOTE_READY;
		if (!ready) return;
		try {
			writeReadyFile(ready);
		} catch {
			return;
		}
	};
}

export function applyRemoteAttach(env: RemoteEnv = process.env): void {
	if (env.CLOUDTHINKER_REMOTE !== "1") return;
	wrapRemoteAttach(InteractiveMode.prototype as unknown as RemoteAttachPrototype, env);
}
