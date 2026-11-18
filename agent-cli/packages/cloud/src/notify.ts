export const NOTIFY_MAX_CHARS = 200;

export type NotifyEnv = Readonly<Record<string, string | undefined>>;

export interface NotifyOutput {
	isTTY?: boolean;
	write(data: string): unknown;
}

export type Focus = "focused" | "unfocused" | "unknown";
export type NotifyProtocol = "osc9" | "osc99" | "osc777";

export const FOCUS_REPORTING_ON = "\x1b[?1004h";
export const FOCUS_REPORTING_OFF = "\x1b[?1004l";
const FOCUS_IN = "\x1b[I";
const FOCUS_OUT = "\x1b[O";
const BEL = "\x07";

const CONTROL = /[\u0000-\u001f\u007f-\u009f]/g;

let focus: Focus = "unknown";
let enabled = true;

export function focusEvent(data: string): Exclude<Focus, "unknown"> | undefined {
	if (data === FOCUS_IN) return "focused";
	if (data === FOCUS_OUT) return "unfocused";
	return undefined;
}

export function terminalFocus(): Focus {
	return focus;
}

export function setTerminalFocus(next: Focus): void {
	focus = next;
}

export function notificationsEnabled(): boolean {
	return enabled;
}

export function setNotificationsEnabled(next: boolean): void {
	enabled = next;
}

export function notifyProtocol(env: NotifyEnv): NotifyProtocol {
	const term = env.TERM ?? "";
	if (term === "xterm-kitty" || env.KITTY_WINDOW_ID !== undefined || env.TERM_PROGRAM === "kitty") return "osc99";
	if (term.startsWith("foot") || term.includes("rxvt") || env.TERM_PROGRAM === "foot") return "osc777";
	return "osc9";
}

export function notificationSequence(text: string, env: NotifyEnv): string {
	const body = Array.from(text.replace(CONTROL, " ").trim()).slice(0, NOTIFY_MAX_CHARS).join("");
	const protocol = notifyProtocol(env);
	const sequence = protocol === "osc99"
		? `\x1b]99;;${body}\x1b\\`
		: protocol === "osc777"
			? `\x1b]777;notify;CloudThinker;${body.replaceAll(";", ",")}${BEL}`
			: `\x1b]9;${body}${BEL}`;
	return BEL + (env.TMUX === undefined ? sequence : `\x1bPtmux;${sequence.replaceAll("\x1b", "\x1b\x1b")}\x1b\\`);
}

export function terminalNotify(
	text: string,
	env: NotifyEnv = process.env,
	output: NotifyOutput = process.stdout,
	state: Focus = focus,
): void {
	if (!output.isTTY || !enabled || env.CLOUDTHINKER_NOTIFY === "0" || state !== "unfocused") return;
	output.write(notificationSequence(text, env));
}
