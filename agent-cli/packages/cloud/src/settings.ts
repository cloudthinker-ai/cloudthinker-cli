import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import { SettingsManager, getAgentDir } from "@earendil-works/pi-coding-agent";

export const CLOUD_DEFAULT_KEY = "cloudDefault";

export function cloudDefaultEnabled(cwd: string, projectTrusted = true): boolean {
	try {
		const settings = SettingsManager.create(cwd, getAgentDir(), { projectTrusted });
		const merged = {
			...settings.getGlobalSettings(),
			...settings.getProjectSettings(),
		} as Record<string, unknown>;
		return merged[CLOUD_DEFAULT_KEY] !== false;
	} catch {
		return true;
	}
}

export function resolveCloudEnabled(
	recorded: unknown,
	option: boolean | undefined,
	setting: boolean,
): boolean {
	if (option === false) return false;
	if (typeof recorded === "boolean") return recorded;
	return option ?? setting;
}

const INTRO_FILE = "intro-version";

export function isMissingFile(error: unknown): boolean {
	return error instanceof Error && "code" in error && error.code === "ENOENT";
}

export function introDue(version: string, everyLaunch: boolean): boolean {
	const agentDir = getAgentDir();
	try {
		if (!SettingsManager.create(process.cwd(), agentDir).getIntroAnimation()) return false;
	} catch {
		return false;
	}
	if (everyLaunch) return true;
	try {
		return readFileSync(join(agentDir, INTRO_FILE), "utf8") !== version;
	} catch (error) {
		return isMissingFile(error);
	}
}

export function recordIntro(version: string): void {
	const agentDir = getAgentDir();
	try {
		mkdirSync(agentDir, { recursive: true });
		writeFileSync(join(agentDir, INTRO_FILE), version);
	} catch {
		return;
	}
}
