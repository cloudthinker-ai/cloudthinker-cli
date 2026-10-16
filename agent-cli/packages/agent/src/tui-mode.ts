import type { SettingsManager } from "@earendil-works/pi-coding-agent";

import { terminalOnlyClipboard } from "./terminal-copy.ts";

export const TUI_MODE_FLAG = "--tui-mode";
export const DEFAULT_TUI_MODE = "fullscreen";
export const REMOTE_TUI_MODE = "regular";

export function savedTuiMode(settings: Pick<SettingsManager, "getGlobalSettings" | "getProjectSettings">): unknown {
	const merged = { ...settings.getGlobalSettings(), ...settings.getProjectSettings() } as Record<string, unknown>;
	return merged.tuiMode;
}

export function tuiModeArgs(argv: readonly string[], saved: unknown, env: NodeJS.ProcessEnv = process.env, platform: NodeJS.Platform = process.platform): string[] {
	if (argv.includes(TUI_MODE_FLAG) || saved !== undefined) return [];
	return [TUI_MODE_FLAG, terminalOnlyClipboard(env, platform) ? REMOTE_TUI_MODE : DEFAULT_TUI_MODE];
}
