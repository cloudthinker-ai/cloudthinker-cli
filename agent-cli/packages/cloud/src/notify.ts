export const NOTIFY_MAX_CHARS = 200;

export type NotifyEnv = Readonly<Record<string, string | undefined>>;

export interface NotifyOutput {
	isTTY?: boolean;
	write(data: string): unknown;
}

const CONTROL = /[\u0000-\u001f\u007f-\u009f]/g;

export function notificationSequence(text: string, env: NotifyEnv): string {
	const body = Array.from(text.replace(CONTROL, " ").trim()).slice(0, NOTIFY_MAX_CHARS).join("");
	const kitty = env.TERM === "xterm-kitty" || env.KITTY_WINDOW_ID !== undefined;
	const sequence = kitty ? `\x1b]99;;${body}\x1b\\` : `\x1b]9;${body}\x07`;
	return env.TMUX === undefined ? sequence : `\x1bPtmux;${sequence.replaceAll("\x1b", "\x1b\x1b")}\x1b\\`;
}

export function terminalNotify(text: string, env: NotifyEnv = process.env, output: NotifyOutput = process.stdout): void {
	if (!output.isTTY || env.CLOUDTHINKER_NOTIFY === "0") return;
	output.write(notificationSequence(text, env));
}
