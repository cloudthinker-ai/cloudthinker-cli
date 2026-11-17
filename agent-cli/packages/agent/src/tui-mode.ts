import type { SettingsManager } from "@earendil-works/pi-coding-agent";
import { mergedSettings } from "./pi-settings.ts";

export const TUI_MODE_FLAG = "--tui-mode";
export const DEFAULT_TUI_MODE = "fullscreen";

export function savedTuiMode(settings: Pick<SettingsManager, "getGlobalSettings" | "getProjectSettings">): unknown {
	return mergedSettings(settings).tuiMode;
}

export function tuiModeArgs(argv: readonly string[], saved: unknown): string[] {
	if (argv.includes(TUI_MODE_FLAG) || saved !== undefined) return [];
	return [TUI_MODE_FLAG, DEFAULT_TUI_MODE];
}
