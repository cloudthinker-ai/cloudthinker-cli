import { existsSync } from "node:fs";
import { join } from "node:path";

export const DEFAULT_THEME = "cloudthinker-light/cloudthinker-dark";
export const THEME_NAMES: readonly string[] = ["cloudthinker-dark", "cloudthinker-light"];

export function bundledThemePaths(
	themeDir: string,
	exists: (path: string) => boolean = existsSync,
): string[] {
	return THEME_NAMES.map((name) => join(themeDir, `${name}.json`)).filter(exists);
}

export function themeArgs(
	paths: string[],
	argv: string[],
	savedTheme: string | undefined,
): string[] {
	if (paths.length === 0 || argv.includes("--no-themes")) return [];
	const args = paths.flatMap((path) => ["--theme", path]);
	if (!argv.includes("--use-theme") && savedTheme === undefined) {
		args.push("--use-theme", DEFAULT_THEME);
	}
	return args;
}
