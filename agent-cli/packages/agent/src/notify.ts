import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

import { terminalNotify } from "@cloudthinker/cloud/src/notify.ts";

export const LONG_TURN_MS = 10_000;

export function turnDoneText(elapsedMs: number): string {
	return `CloudThinker: done after ${Math.round(elapsedMs / 1000)}s`;
}

export function registerTurnNotify(
	pi: Pick<ExtensionAPI, "on">,
	now: () => number = Date.now,
	notify: (text: string) => void = terminalNotify,
): void {
	let startedAt: number | undefined;
	pi.on("agent_start", (_event, ctx) => {
		startedAt = ctx.mode === "tui" && ctx.hasUI ? now() : undefined;
	});
	pi.on("agent_end", () => {
		if (startedAt === undefined) return;
		const elapsed = now() - startedAt;
		startedAt = undefined;
		if (elapsed >= LONG_TURN_MS) notify(turnDoneText(elapsed));
	});
}
