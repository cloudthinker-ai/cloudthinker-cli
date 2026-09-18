import { keyHint, keyText, rawKeyHint } from "@earendil-works/pi-coding-agent";
import type { Theme, ThemeColor } from "@earendil-works/pi-coding-agent";
import { Text } from "@earendil-works/pi-tui";
import type { TUI } from "@earendil-works/pi-tui";

import { sanitizeTerminalText } from "./awareness.ts";
import { renderLogo } from "./logo.ts";
import { PI_AUTHOR, PI_LICENSE, type HostVersions } from "./versions.ts";

export const PRODUCT_NAME = "cloudthinker";
export const UNLINKED_LINE = "not linked · cloud tools unavailable · restart to retry";
export const LINKING_LINE = "linking…";

export type LinkState = "linking" | "linked" | "unavailable";

export interface HeaderState {
	link: LinkState;
	workspaceName?: string;
	userEmail?: string;
	webUrl?: string;
	autoMode?: boolean;
}

export const AUTO_LABEL = "Auto";
export const MANUAL_LABEL = "Manual";

export interface HeaderStyler {
	fg(color: ThemeColor, text: string): string;
	bold(text: string): string;
	getFgAnsi?(color: ThemeColor): string;
}

export interface HeaderHints {
	compact: string;
	expanded: string;
	more: string;
}

function formatSessionLine(state: HeaderState, styler: HeaderStyler): string {
	if (state.link === "unavailable") return styler.fg("error", UNLINKED_LINE);
	if (state.link === "linking") return styler.fg("dim", LINKING_LINE);
	const parts: string[] = [];
	if (state.workspaceName) parts.push(styler.fg("text", `${sanitizeTerminalText(state.workspaceName)} (cloud)`));
	if (state.autoMode !== undefined) {
		parts.push(styler.fg("muted", state.autoMode ? AUTO_LABEL : MANUAL_LABEL));
	}
	return parts.join(styler.fg("muted", " · "));
}

export function buildHints(styler: HeaderStyler): HeaderHints {
	const separator = styler.fg("muted", " · ");
	return {
		compact: [
			keyHint("app.interrupt", "interrupt"),
			rawKeyHint(`${keyText("app.clear")}/${keyText("app.exit")}`, "clear/exit"),
			rawKeyHint("/", "commands"),
			rawKeyHint("!", "bash"),
			keyHint("app.tools.expand", "more"),
		].join(separator),
		expanded: [
			keyHint("app.interrupt", "to interrupt"),
			keyHint("app.clear", "to clear"),
			rawKeyHint(`${keyText("app.clear")} twice`, "to exit"),
			keyHint("app.exit", "to exit (empty)"),
			keyHint("app.suspend", "to suspend"),
			keyHint("tui.editor.deleteToLineEnd", "to delete to end"),
			rawKeyHint(
				`${keyText("app.model.cycleForward")}/${keyText("app.model.cycleBackward")}`,
				"to cycle models",
			),
			keyHint("app.model.select", "to select model"),
			keyHint("app.tools.expand", "to expand tools"),
			keyHint("app.editor.external", "for external editor"),
			rawKeyHint("/", "for commands"),
			rawKeyHint("!", "to run bash"),
			rawKeyHint("!!", "to run bash (no context)"),
			keyHint("app.message.followUp", "to queue follow-up"),
			keyHint("app.message.dequeue", "to edit all queued messages"),
			keyHint("app.clipboard.pasteImage", "to paste image (with text fallback)"),
			rawKeyHint("drop files", "to attach"),
		].join("\n"),
		more: styler.fg("muted", `Press ${keyText("app.tools.expand")} for startup details`),
	};
}

export function formatHeaderText(
	state: HeaderState,
	versions: HostVersions,
	styler: HeaderStyler,
	hints: HeaderHints,
	expanded: boolean,
	width = 80,
): string {
	const title = `${styler.bold(styler.fg("accent", "CloudThinker"))}${styler.fg("muted", ` v${versions.host}`)}`;
	const identity = [title];
	if (state.link === "linked") {
		identity.push(formatSessionLine(state, styler));
		if (state.userEmail) identity.push(styler.fg("muted", sanitizeTerminalText(state.userEmail)));
		if (state.webUrl) identity.push(styler.fg("muted", "/open · view session in browser"));
	} else {
		identity.push(formatSessionLine(state, styler));
	}
	const lines: string[] = [];
	if (width >= 76) {
		const logo = renderLogo(styler, process.env.NO_COLOR !== undefined);
		const details = new Text(identity.join("\n"), 0, 0).render(width);
		lines.push(...logo, "", ...details);
	} else {
		lines.push(...identity);
	}
	if (versions.pi) lines.push(styler.fg("muted", `built on pi v${versions.pi} by ${PI_AUTHOR} (${PI_LICENSE})`));
	if (expanded && state.webUrl) lines.push(styler.fg("muted", sanitizeTerminalText(state.webUrl)));
	if (expanded) return [...lines, hints.expanded].join("\n");
	return [...lines, hints.compact, hints.more].join("\n");
}

class HeaderComponent extends Text {
	private expanded = false;
	private readonly build: (expanded: boolean, width: number) => string;
	private width = 80;

	constructor(build: (expanded: boolean, width: number) => string) {
		super(build(false, 78), 1, 0);
		this.build = build;
	}

	setExpanded(expanded: boolean): void {
		this.expanded = expanded;
		this.refresh();
	}

	refresh(): void {
		this.setText(this.build(this.expanded, Math.max(1, this.width - 2)));
	}

	override render(width: number): string[] {
		if (width !== this.width) {
			this.width = width;
			this.refresh();
		}
		return super.render(width);
	}
}

export class SessionHeader {
	private state: HeaderState = { link: "linking" };
	private component: HeaderComponent | undefined;
	private tui: TUI | undefined;
	private readonly versions: HostVersions;

	constructor(versions: HostVersions) {
		this.versions = versions;
	}

	factory = (tui: TUI, theme: Theme): HeaderComponent => {
		const hints = buildHints(theme);
		this.tui = tui;
		this.component = new HeaderComponent((expanded, width) =>
			formatHeaderText(this.state, this.versions, theme, hints, expanded, width),
		);
		return this.component;
	};

	reset(): void {
		this.set({
			link: "linking",
			workspaceName: undefined,
			userEmail: undefined,
			webUrl: undefined,
			autoMode: undefined,
		});
	}

	set(next: Partial<HeaderState>): void {
		this.state = { ...this.state, ...next };
		this.component?.refresh();
		this.tui?.requestRender();
	}
}
