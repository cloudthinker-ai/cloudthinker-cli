import type { Theme, ThemeColor } from "@earendil-works/pi-coding-agent";
import { type Component, visibleWidth } from "@earendil-works/pi-tui";

export const LOCAL_TAG = "[L]";
export const CLOUD_TAG = "[C]";
export const BOTH_TAG = "[L+C]";
export const TAG_LEGEND = "[L] your machine - [C] CloudThinker Sandbox - /cloud off keeps work local";
export const IDENTITY_UNKNOWN = "unknown (identity call failed)";

export const LOCAL_TOOLS: string[] = ["bash", "read", "write", "edit", "grep", "find", "ls", "powershell"];

// Strip OSC/CSI/other ANSI escape sequences, then zero-width and bidi format
// characters, then any remaining C0/C1 control character (including line breaks)
// becomes a space, so untrusted text cannot rewrite the terminal, hide behind
// invisible characters, or draw a fake legend or badge line.
const TERMINAL_ESCAPE = /\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[PX^_][\s\S]*?\x1b\\|\x1b\[[0-?]*[ -/]*[@-~]|\x1b[@-Z\\-_]/g;
const INVISIBLE_FORMAT = /[\u061c\u180e\u200b-\u200f\u202a-\u202e\u2060-\u2064\u2066-\u2069\ufeff]/g;
const CONTROL_CHAR = /[\u0000-\u001f\u007f-\u009f]/g;

export function sanitizeTerminalText(text: string): string {
	return text.replace(TERMINAL_ESCAPE, "").replace(INVISIBLE_FORMAT, "").replace(CONTROL_CHAR, " ");
}

let machineState: MachineState = {
	cwd: process.cwd(),
	linked: false,
	connectionCount: 0,
	cloudEnabled: true,
};

export interface Legend {
	reset(): void;
	tag(toolCallId: string | undefined): string | undefined;
}

// One legend owner per session, held by that session's runtime, so a parent and
// a delegated child never claim or suppress each other's first tagged call.
export function createLegend(): Legend {
	let owner: string | undefined;
	return {
		reset(): void {
			owner = undefined;
		},
		tag(toolCallId: string | undefined): string | undefined {
			if (!toolCallId) return undefined;
			owner ??= toolCallId;
			return owner === toolCallId ? TAG_LEGEND : undefined;
		},
	};
}

export const MISSING_LOCAL_FILE_HINT =
	"That path was looked for on this machine; CloudThinker Sandbox files are not visible from here.";

const MISSING_FILE = /no such file|file not found|path not found|directory not found|cannot find (module|package|file|path|directory)|does not exist|ENOENT/i;

export function localMissingFile(text: string): boolean {
	return MISSING_FILE.test(text) && !text.includes(MISSING_LOCAL_FILE_HINT);
}

function tagColor(tag: string): ThemeColor {
	return tag === CLOUD_TAG ? "accent" : "muted";
}

export function taggedComponent(
	tag: string,
	theme: Theme,
	component: Component,
	legend?: string,
): Component {
	const prefix = `${theme.fg(tagColor(tag), tag)} `;
	const legendLine = legend ? theme.fg("muted", legend) : undefined;
	return {
		render(width) {
			const inner = Math.max(1, width - visibleWidth(prefix));
			const lines = component.render(inner);
			const tagged = lines.length > 0 ? [`${prefix}${lines[0]}`, ...lines.slice(1)] : [prefix.trimEnd()];
			return legendLine ? [legendLine, ...tagged] : tagged;
		},
		invalidate() {
			component.invalidate();
		},
	};
}

export interface MachineState {
	cwd: string;
	linked: boolean;
	workspaceName?: string;
	connectionCount: number;
	cloudEnabled: boolean;
}

export interface TagStyle {
	local(text: string): string;
	cloud(text: string): string;
}

const PLAIN_TAGS: TagStyle = { local: (text) => text, cloud: (text) => text };

export function setMachineState(next: Partial<MachineState>): void {
	machineState = { ...machineState, ...next };
}

export function machineBarLines(style: TagStyle = PLAIN_TAGS): string[] {
	const state = machineState;
	const local = `${style.local(LOCAL_TAG)} this machine  ${sanitizeTerminalText(state.cwd)}   bash - read - write - edit`;
	let sandbox: string;
	if (!state.cloudEnabled) {
		sandbox = `${style.cloud(CLOUD_TAG)} sandbox       off for this session - /cloud on enables it`;
	} else if (!state.linked) {
		sandbox = `${style.cloud(CLOUD_TAG)} sandbox       not linked - cloud tools unavailable`;
	} else {
		const count = state.connectionCount;
		const workspace = state.workspaceName === undefined ? IDENTITY_UNKNOWN : sanitizeTerminalText(state.workspaceName);
		sandbox = `${style.cloud(CLOUD_TAG)} sandbox       workspace ${workspace} - ${count} connection${count === 1 ? "" : "s"}   ct_sandbox_read/write`;
	}
	return [
		local,
		sandbox,
		`Cloud: ${state.cloudEnabled ? "On" : "Off"} - /cloud on|off - /open to watch in the browser`,
	];
}
