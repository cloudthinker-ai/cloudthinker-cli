import { TuiAltScreen } from "@earendil-works/pi-tui";

export const TERMINAL_COPY_HINT = "paste empty? Hold Shift (Option on Mac) while you select";
export const TERMINAL_COPY_NOTE = `Sent to your terminal · ${TERMINAL_COPY_HINT}`;
const NOTE_DURATION_MS = 5000;

interface FlashScreen {
	flash(message: string, durationMs?: number): void;
}

export function terminalOnlyClipboard(env: NodeJS.ProcessEnv = process.env, platform: NodeJS.Platform = process.platform): boolean {
	if (env.SSH_CONNECTION || env.SSH_CLIENT || env.MOSH_CONNECTION) return true;
	return platform === "linux" && !env.DISPLAY && !env.WAYLAND_DISPLAY && !env.TERMUX_VERSION && !env.WSL_DISTRO_NAME;
}

export function copiedStatus(status: string, env: NodeJS.ProcessEnv = process.env, platform: NodeJS.Platform = process.platform): string {
	return terminalOnlyClipboard(env, platform) ? `${status} through your terminal · ${TERMINAL_COPY_HINT}` : status;
}

export function applyTerminalCopyNote(): void {
	const screen = TuiAltScreen.prototype as unknown as FlashScreen;
	const { flash } = screen;
	if (typeof flash !== "function") throw new Error("pi-tui's fullscreen no longer flashes copy results, so a terminal-only copy cannot say how it was sent");
	screen.flash = function (message, durationMs) {
		if (message === "Copied!" && terminalOnlyClipboard()) return flash.call(this, TERMINAL_COPY_NOTE, NOTE_DURATION_MS);
		return flash.call(this, message, durationMs);
	};
}
