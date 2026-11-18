import { SettingsManager, getAgentDir } from "@earendil-works/pi-coding-agent";

import { notificationsEnabled, setNotificationsEnabled } from "@cloudthinker/cloud/src/notify.ts";

import { SettingsSelectorComponent } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/components/settings-selector.js";
import { NOTIFY_SETTING } from "./notify.ts";

export const NOTIFY_ITEM_ID = "cloudthinker-notifications";
const ON = "on";
const OFF = "off";

interface ListItem {
	id: string;
	label: string;
	description: string;
	currentValue: string;
	values: string[];
}

interface ListState {
	items: ListItem[];
	filteredItems: ListItem[];
	onChange(id: string, value: string): Promise<void> | void;
}

interface SettingsStore {
	globalSettings: Record<string, unknown>;
	markModified(field: string): void;
	save(): void;
	flush(): Promise<void>;
}

export async function saveNotifications(enabled: boolean): Promise<void> {
	const store = SettingsManager.create(process.cwd(), getAgentDir()) as unknown as SettingsStore;
	if (typeof store.markModified !== "function" || typeof store.save !== "function" || typeof store.flush !== "function") {
		throw new Error("pi no longer exposes its settings writer, so the notifications setting cannot be saved");
	}
	store.globalSettings[NOTIFY_SETTING] = enabled;
	store.markModified(NOTIFY_SETTING);
	store.save();
	await store.flush();
}

export function applyNotifySettingGuard(): void {
	const prototype = SettingsSelectorComponent.prototype;
	const getSettingsList = prototype.getSettingsList;
	prototype.getSettingsList = function () {
		const original = getSettingsList.call(this);
		const list = original as unknown as ListState;
		if (list.items.some((item) => item.id === NOTIFY_ITEM_ID)) return original;
		const item: ListItem = {
			id: NOTIFY_ITEM_ID,
			label: "Notifications",
			description: "Alert me when a turn ends or an approval waits while this terminal window is in the background",
			currentValue: notificationsEnabled() ? ON : OFF,
			values: [ON, OFF],
		};
		for (const items of list.filteredItems === list.items ? [list.items] : [list.items, list.filteredItems]) {
			const after = items.findIndex((entry) => entry.id === "intro-animation");
			items.splice(after === -1 ? items.length : after + 1, 0, item);
		}
		const onChange = list.onChange;
		list.onChange = (id, value) => {
			if (id !== NOTIFY_ITEM_ID) return onChange(id, value);
			setNotificationsEnabled(value === ON);
			return saveNotifications(value === ON);
		};
		return original;
	};
}
