import { homedir } from "node:os";
import { relative, sep } from "node:path";

import type { ExtensionContext, ReadonlyFooterDataProvider, Theme, ThemeColor } from "@earendil-works/pi-coding-agent";
import { type Component, truncateToWidth, visibleWidth } from "@earendil-works/pi-tui";

import { sanitizeTerminalText } from "./awareness.ts";
import { formatCredits } from "./credits.ts";
import { AUTO_LABEL, MANUAL_LABEL } from "./header.ts";
import { CLOUD_ENTRY_TYPE, CREDITS_KEY, type CloudThinkerRuntime } from "./runtime.ts";

export const CLOUD_OFF_LABEL = "Cloud off";
export const CONNECTING_LABEL = "connecting…";
export const NO_CONNECTIONS_LABEL = "no Connections";
export const MAX_LISTED_CONNECTIONS = 3;
export const OWN_STATUS_KEYS: readonly string[] = [CLOUD_ENTRY_TYPE, CREDITS_KEY];

export interface StatusLineState {
	cloudEnabled: boolean;
	linked: boolean;
	workspaceName?: string;
	autoMode?: boolean;
	creditsUsed?: number;
	connections: readonly string[];
	failure?: string;
}

export interface StatusStyler {
	fg(color: ThemeColor, text: string): string;
	bold(text: string): string;
}

export function connectionsLabel(prefixes: readonly string[]): string {
	if (prefixes.length === 0) return NO_CONNECTIONS_LABEL;
	const listed = prefixes.slice(0, MAX_LISTED_CONNECTIONS).map(sanitizeTerminalText).join(", ");
	const more = prefixes.length - MAX_LISTED_CONNECTIONS;
	return more > 0 ? `${listed} +${more}` : listed;
}

export function statusLineText(state: StatusLineState, styler: StatusStyler): string {
	const separator = styler.fg("dim", " · ");
	if (!state.cloudEnabled) {
		return [styler.fg("dim", "○"), styler.fg("muted", CLOUD_OFF_LABEL)].join(" ");
	}
	if (!state.linked && state.failure) return styler.fg("error", sanitizeTerminalText(state.failure));
	if (!state.linked) return `${styler.fg("dim", "○")} ${styler.fg("dim", CONNECTING_LABEL)}`;
	const parts: string[] = [];
	if (state.workspaceName) parts.push(styler.fg("text", sanitizeTerminalText(state.workspaceName)));
	if (state.autoMode !== undefined) {
		parts.push(state.autoMode ? styler.fg("warning", AUTO_LABEL) : styler.fg("success", MANUAL_LABEL));
	}
	if (state.creditsUsed !== undefined) parts.push(styler.fg("muted", formatCredits(state.creditsUsed)));
	parts.push(styler.fg("muted", connectionsLabel(state.connections)));
	return `${styler.fg("accent", "●")} ${parts.join(separator)}`;
}

export function shortCwd(cwd: string, home = homedir()): string {
	const inside = relative(home, cwd);
	if (inside === "") return "~";
	if (inside.startsWith("..") || inside.startsWith(sep)) return cwd;
	return `~${sep}${inside}`;
}

export function joinEnds(left: string, right: string, width: number): string {
	const leftWidth = visibleWidth(left);
	const rightWidth = visibleWidth(right);
	if (leftWidth + 2 + rightWidth > width) return truncateToWidth(left, width);
	return `${left}${" ".repeat(width - leftWidth - rightWidth)}${right}`;
}

export function runtimeStatusState(runtime: CloudThinkerRuntime): StatusLineState {
	return {
		cloudEnabled: runtime.cloudEnabled,
		linked: runtime.session !== undefined,
		workspaceName: runtime.identity?.workspace_name,
		autoMode: runtime.autoMode?.enabled,
		creditsUsed: runtime.credits?.credits_used,
		connections: runtime.connectedPrefixes,
		failure: runtime.linkFailure?.label,
	};
}

export function contextLabel(ctx: Pick<ExtensionContext, "getContextUsage" | "model">): string {
	const parts: string[] = [];
	const mode = ctx.model?.name ?? ctx.model?.id;
	if (mode) parts.push(mode);
	const percent = ctx.getContextUsage()?.percent;
	if (percent !== null && percent !== undefined) parts.push(`${Math.round(percent)}% context`);
	return parts.join(" · ");
}

export function statusLineFooter(
	runtime: CloudThinkerRuntime,
	ctx: Pick<ExtensionContext, "cwd" | "getContextUsage" | "model">,
): (tui: unknown, theme: Theme, footerData: ReadonlyFooterDataProvider) => Component {
	return (_tui, theme, footerData) => ({
		render(width: number): string[] {
			const branch = footerData.getGitBranch();
			const where = theme.fg("dim", `${shortCwd(ctx.cwd)}${branch ? ` (${branch})` : ""}`);
			const first = joinEnds(` ${statusLineText(runtimeStatusState(runtime), theme)}`, `${where} `, width);
			const others = [...footerData.getExtensionStatuses().entries()]
				.filter(([key]) => !OWN_STATUS_KEYS.includes(key))
				.sort(([a], [b]) => a.localeCompare(b))
				.map(([, text]) => text.replace(/[\r\n\t]+/g, " ").trim())
				.filter((text) => text.length > 0);
			const context = contextLabel(ctx);
			const second = joinEnds(` ${theme.fg("muted", others.join(" · "))}`, `${theme.fg("dim", context)} `, width);
			return [first, others.length > 0 || context ? second : ""].filter((line) => line.length > 0);
		},
		invalidate() {},
	});
}
