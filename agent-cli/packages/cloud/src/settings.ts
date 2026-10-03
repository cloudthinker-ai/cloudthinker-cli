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
