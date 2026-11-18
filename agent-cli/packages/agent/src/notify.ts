import { SettingsManager, getAgentDir, type ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { TuiAltScreen } from "@earendil-works/pi-tui";

import {
	FOCUS_REPORTING_OFF,
	FOCUS_REPORTING_ON,
	focusEvent,
	setNotificationsEnabled,
	setTerminalFocus,
	terminalNotify,
} from "@cloudthinker/cloud/src/notify.ts";

import { readSetting } from "./pi-settings.ts";

export const NOTIFY_SETTING = "notifications";

export function turnDoneText(elapsedMs: number): string {
	return `CloudThinker: done after ${Math.round(elapsedMs / 1000)}s`;
}

export function configuredNotifications(cwd: string, projectTrusted: boolean): boolean {
	const settings = SettingsManager.create(cwd, getAgentDir(), { projectTrusted });
	return readSetting(settings, NOTIFY_SETTING, (value): value is boolean => typeof value === "boolean", true);
}

interface FocusScreen {
	handleViewportInput(data: string): unknown;
}

export function applyFocusTracking(): void {
	const screen = TuiAltScreen.prototype as unknown as FocusScreen;
	const { handleViewportInput } = screen;
	if (typeof handleViewportInput !== "function") {
		throw new Error("pi-tui's fullscreen no longer reads focus reports in handleViewportInput, so a turn-end notification cannot know the window is away");
	}
	screen.handleViewportInput = function (data) {
		const next = focusEvent(data);
		if (next !== undefined) setTerminalFocus(next);
		return handleViewportInput.call(this, data);
	};
}

interface FocusOutput {
	isTTY?: boolean;
	write(data: string): unknown;
}

export function registerNotifications(
	pi: Pick<ExtensionAPI, "on">,
	now: () => number = Date.now,
	notify: (text: string) => void = terminalNotify,
	output: FocusOutput = process.stdout,
): void {
	let startedAt: number | undefined;
	let stopListening: (() => void) | undefined;
	pi.on("session_start", (_event, ctx) => {
		stopListening?.();
		stopListening = undefined;
		if (ctx.mode !== "tui" || !ctx.hasUI) return;
		setNotificationsEnabled(configuredNotifications(ctx.cwd, ctx.isProjectTrusted()));
		if (!output.isTTY) return;
		output.write(FOCUS_REPORTING_ON);
		stopListening = ctx.ui.onTerminalInput((data) => {
			const next = focusEvent(data);
			if (next === undefined) return undefined;
			setTerminalFocus(next);
			return { consume: true };
		});
	});
	pi.on("session_shutdown", () => {
		if (stopListening === undefined) return;
		stopListening();
		stopListening = undefined;
		output.write(FOCUS_REPORTING_OFF);
	});
	pi.on("agent_start", (_event, ctx) => {
		startedAt = ctx.mode === "tui" && ctx.hasUI ? now() : undefined;
	});
	pi.on("agent_end", () => {
		if (startedAt === undefined) return;
		const elapsed = now() - startedAt;
		startedAt = undefined;
		notify(turnDoneText(elapsed));
	});
}
