import { SettingsManager, getAgentDir, type ExtensionAPI } from "@earendil-works/pi-coding-agent";

export const TOOL_OUTPUT_SETTING = "toolOutput";
export const TOOL_OUTPUT_MODES = ["compact", "preview"] as const;
export type ToolOutputMode = (typeof TOOL_OUTPUT_MODES)[number];

export const DIFF_STYLE_SETTING = "diffStyle";
export const DIFF_STYLES = ["auto", "unified", "split"] as const;
export type DiffStyle = (typeof DIFF_STYLES)[number];

let mode: ToolOutputMode = "compact";
let style: DiffStyle = "auto";

export function diffStyle(): DiffStyle {
	return style;
}

export function setDiffStyle(next: DiffStyle): void {
	style = next;
}

export function configuredDiffStyle(cwd: string, projectTrusted: boolean): DiffStyle {
	const settings = SettingsManager.create(cwd, getAgentDir(), { projectTrusted });
	const merged = { ...settings.getGlobalSettings(), ...settings.getProjectSettings() } as Record<string, unknown>;
	const value = merged[DIFF_STYLE_SETTING];
	return DIFF_STYLES.includes(value as DiffStyle) ? (value as DiffStyle) : "auto";
}

export function toolOutputMode(): ToolOutputMode {
	return mode;
}

export function setToolOutputMode(next: ToolOutputMode): void {
	mode = next;
}

function isMode(value: unknown): value is ToolOutputMode {
	return TOOL_OUTPUT_MODES.includes(value as ToolOutputMode);
}

export function configuredToolOutputMode(cwd: string, projectTrusted: boolean): ToolOutputMode {
	const settings = SettingsManager.create(cwd, getAgentDir(), { projectTrusted });
	const merged = { ...settings.getGlobalSettings(), ...settings.getProjectSettings() } as Record<string, unknown>;
	const value = merged[TOOL_OUTPUT_SETTING];
	return isMode(value) ? value : "compact";
}

export function registerVerbosity(pi: ExtensionAPI): void {
	pi.on("session_start", async (_event, ctx) => {
		setToolOutputMode(configuredToolOutputMode(ctx.cwd, ctx.isProjectTrusted()));
		setDiffStyle(configuredDiffStyle(ctx.cwd, ctx.isProjectTrusted()));
	});
	pi.registerCommand("verbosity", {
		description: "Show tool output as a one-line summary (compact) or a short preview",
		getArgumentCompletions: (prefix) =>
			TOOL_OUTPUT_MODES.filter((name) => name.startsWith(prefix)).map((name) => ({ value: name, label: name })),
		handler: async (args, ctx) => {
			const argument = args.trim();
			if (argument && !isMode(argument)) {
				ctx.ui.notify(`Usage: /verbosity [${TOOL_OUTPUT_MODES.join("|")}]`, "warning");
				return;
			}
			setToolOutputMode(isMode(argument) ? argument : TOOL_OUTPUT_MODES[(TOOL_OUTPUT_MODES.indexOf(mode) + 1) % TOOL_OUTPUT_MODES.length]!);
			const expanded = ctx.ui.getToolsExpanded();
			ctx.ui.setToolsExpanded(!expanded);
			ctx.ui.setToolsExpanded(expanded);
			ctx.ui.notify(
				`Tool output: ${mode}. Expand tool output to see everything. Set "${TOOL_OUTPUT_SETTING}": "${mode}" in settings.json to keep it.`,
				"info",
			);
		},
	});
}
