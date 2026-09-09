export const DEFAULT_TUI_MODE = "fullscreen";

export function tuiModeArgs(argv: string[]): string[] {
	return argv.includes("--tui-mode") ? [] : ["--tui-mode", DEFAULT_TUI_MODE];
}
