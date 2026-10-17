import { TuiAltScreen } from "@earendil-works/pi-tui";

const NOTE_DURATION_MS = 5000;

interface CopyScreen {
	flash(message: string, durationMs?: number): void;
	copyTextToClipboard(text: string): Promise<boolean>;
}

export function terminalOnlyClipboard(env: NodeJS.ProcessEnv = process.env, platform: NodeJS.Platform = process.platform): boolean {
	if (env.SSH_CONNECTION || env.SSH_CLIENT || env.MOSH_CONNECTION) return true;
	return platform === "linux" && !env.DISPLAY && !env.WAYLAND_DISPLAY && !env.TERMUX_VERSION && !env.WSL_DISTRO_NAME;
}

export function nativeSelectKey(env: NodeJS.ProcessEnv = process.env): string {
	if (env.TERM_PROGRAM === "Apple_Terminal") return "Fn";
	if (env.TERM_PROGRAM === "iTerm.app" || env.LC_TERMINAL === "iTerm2") return "Option";
	if (env.TERM_PROGRAM) return "Shift";
	return "Shift (Option in iTerm2, Fn in Terminal.app)";
}

export function terminalCopyHint(env: NodeJS.ProcessEnv = process.env): string {
	return `if paste fails, hold ${nativeSelectKey(env)} while selecting`;
}

function counted(chars: number | undefined): string {
	return chars === undefined ? "" : ` ${chars} ${chars === 1 ? "char" : "chars"}`;
}

export function localCopyNote(chars: number | undefined): string {
	return `Copied${counted(chars)} to clipboard`;
}

export function terminalCopyNote(chars: number | undefined, env: NodeJS.ProcessEnv = process.env): string {
	return `Sent${counted(chars)} via OSC 52 · ${terminalCopyHint(env)}`;
}

export function copiedStatus(status: string, env: NodeJS.ProcessEnv = process.env, platform: NodeJS.Platform = process.platform): string {
	return terminalOnlyClipboard(env, platform) ? `${status} via OSC 52 · ${terminalCopyHint(env)}` : status;
}

export function applyTerminalCopyNote(): void {
	const screen = TuiAltScreen.prototype as unknown as CopyScreen;
	const { flash, copyTextToClipboard } = screen;
	if (typeof flash !== "function" || typeof copyTextToClipboard !== "function") {
		throw new Error("pi-tui's fullscreen no longer copies through copyTextToClipboard and flash, so a terminal-only copy cannot say how it was sent");
	}
	const copying = new WeakMap<object, number>();
	screen.copyTextToClipboard = async function (text) {
		copying.set(this, [...text].length);
		try {
			return await copyTextToClipboard.call(this, text);
		} finally {
			copying.delete(this);
		}
	};
	screen.flash = function (message, durationMs) {
		if (message !== "Copied!") return flash.call(this, message, durationMs);
		if (terminalOnlyClipboard()) return flash.call(this, terminalCopyNote(copying.get(this)), NOTE_DURATION_MS);
		return flash.call(this, localCopyNote(copying.get(this)), durationMs);
	};
}
