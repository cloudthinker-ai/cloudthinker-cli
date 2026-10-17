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

export function plural(count: number, one: string): string {
	return `${count} ${count === 1 ? one : `${one}s`}`;
}

function what(label: string | undefined): string {
	return label ? ` ${label}` : "";
}

export function localCopyNote(label: string | undefined): string {
	return `Copied${what(label)} to clipboard`;
}

export function terminalCopyNote(label: string | undefined, env: NodeJS.ProcessEnv = process.env): string {
	return `Sent${what(label)} via OSC 52 · ${terminalCopyHint(env)}`;
}

const labels = new WeakMap<object, { text: string; label: string }>();

export function labelNextCopy(screen: object, text: string, label: string): void {
	labels.set(screen, { text, label });
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
	const copying = new WeakMap<object, string>();
	screen.copyTextToClipboard = async function (text) {
		const pending = labels.get(this);
		labels.delete(this);
		copying.set(this, pending?.text === text ? pending.label : plural([...text].length, "char"));
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
