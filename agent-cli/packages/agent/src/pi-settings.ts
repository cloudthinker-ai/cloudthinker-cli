import { SettingsManager, getAgentDir } from "@earendil-works/pi-coding-agent";

export function mergedSettings(
	settings: Pick<SettingsManager, "getGlobalSettings" | "getProjectSettings">,
): Record<string, unknown> {
	return { ...settings.getGlobalSettings(), ...settings.getProjectSettings() } as Record<string, unknown>;
}

export function readSetting<T>(
	settings: Pick<SettingsManager, "getGlobalSettings" | "getProjectSettings">,
	key: string,
	isValid: (value: unknown) => value is T,
	fallback: T,
): T {
	const value = mergedSettings(settings)[key];
	return isValid(value) ? value : fallback;
}

export function readAgentSetting<T>(
	cwd: string,
	projectTrusted: boolean,
	key: string,
	isValid: (value: unknown) => value is T,
	fallback: T,
): T {
	const settings = SettingsManager.create(cwd, getAgentDir(), { projectTrusted });
	return readSetting(settings, key, isValid, fallback);
}
