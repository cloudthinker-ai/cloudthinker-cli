import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { stripVTControlCharacters } from "node:util";
import test from "node:test";

import { notificationsEnabled, setNotificationsEnabled } from "@cloudthinker/cloud/src/notify.ts";

import { SettingsSelectorComponent } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/components/settings-selector.js";
import { initTheme } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/theme/theme.js";
import { NOTIFY_ITEM_ID, applyNotifySettingGuard } from "../src/notify-setting.ts";

initTheme("light");

test("/settings lists Notifications, and turning it off stops notifications and is saved", async () => {
	const agentDir = mkdtempSync(join(tmpdir(), "ct-notify-setting-"));
	const previous = process.env.PI_CODING_AGENT_DIR;
	process.env.PI_CODING_AGENT_DIR = agentDir;
	try {
		applyNotifySettingGuard();
		setNotificationsEnabled(true);
		const config = {
			availableDefaultModels: [], availableThemes: ["light"], modelThinkingLevels: {},
			currentTheme: "light", httpIdleTimeoutMs: 60_000, thinkingLevel: "medium",
		} as unknown as ConstructorParameters<typeof SettingsSelectorComponent>[0];
		const component = new SettingsSelectorComponent(config, { onCancel: () => undefined } as unknown as ConstructorParameters<typeof SettingsSelectorComponent>[1]);
		const list = component.getSettingsList();
		assert.equal(component.getSettingsList(), list);
		const state = list as unknown as { items: { id: string; currentValue: string }[]; onChange(id: string, value: string): Promise<void> };
		assert.equal(state.items.filter((item) => item.id === NOTIFY_ITEM_ID).length, 1);
		assert.equal(state.items.find((item) => item.id === NOTIFY_ITEM_ID)?.currentValue, "on");
		list.handleInput("notif");
		assert.match(stripVTControlCharacters(list.render(100).join("\n")), /Notifications\s+on/);
		await state.onChange(NOTIFY_ITEM_ID, "off");
		assert.equal(notificationsEnabled(), false);
		assert.equal((JSON.parse(readFileSync(join(agentDir, "settings.json"), "utf8")) as { notifications?: boolean }).notifications, false);
		await state.onChange(NOTIFY_ITEM_ID, "on");
		assert.equal(notificationsEnabled(), true);
		assert.equal((JSON.parse(readFileSync(join(agentDir, "settings.json"), "utf8")) as { notifications?: boolean }).notifications, true);
	} finally {
		setNotificationsEnabled(true);
		if (previous === undefined) delete process.env.PI_CODING_AGENT_DIR;
		else process.env.PI_CODING_AGENT_DIR = previous;
		rmSync(agentDir, { recursive: true, force: true });
	}
});
