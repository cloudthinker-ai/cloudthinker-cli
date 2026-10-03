import { InteractiveMode } from "@earendil-works/pi-coding-agent";

import { FooterComponent } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/components/footer.js";
import { SettingsSelectorComponent } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/components/settings-selector.js";
import { CLEAR_SCREEN } from "./clear-screen.ts";
import { PROMPT_SEARCH } from "./prompt-search.ts";

interface InteractivePrototype {
	cycleThinkingLevel(): void;
	showStatus(message: string): void;
	handleHotkeysCommand(): void;
	getAppKeyDisplay(action: string): string;
	chatContainer: { addChild(child: { text?: string; setText?(text: string): void }): void };
}

export function withoutThinkingStatus(message: string): string {
	const visible = message.replace(/ \(thinking: [^)]+\)$/, "");
	if (/\bthinking\s*:/i.test(visible)) {
		throw new Error("pi's thinking status format changed");
	}
	return visible;
}

export const HOTKEY_ROWS: Readonly<Record<string, string | undefined>> = {
	"Cycle thinking level": undefined,
	"Open model selector": "Clear the screen",
	"Toggle tool output expansion": "Open the transcript",
	"Toggle thinking block visibility": "Show or hide thinking",
};

export function cloudThinkerHotkeys(text: string, keys: Readonly<Record<string, string>> = {}): string {
	const seen = new Set<string>();
	const lines: string[] = [];
	for (const line of text.split("\n")) {
		const match = /^(\|.*\| )(.+)( \|)$/.exec(line);
		const action = match?.[2];
		if (match && action !== undefined && action in HOTKEY_ROWS) {
			seen.add(action);
			const replacement = HOTKEY_ROWS[action];
			const key = replacement === undefined ? undefined : keys[replacement];
			if (key !== undefined) lines.push(`| \`${key}\` | ${replacement} |`);
			if (replacement === "Clear the screen" && keys["Search past prompts"] !== undefined) lines.push(`| \`${keys["Search past prompts"]}\` | Search past prompts |`);
			else if (replacement !== undefined) lines.push(`${match[1]}${replacement}${match[3]}`);
			continue;
		}
		lines.push(line);
	}
	const visible = lines.join("\n");
	if (seen.size !== Object.keys(HOTKEY_ROWS).length || /thinking level/i.test(visible)) {
		throw new Error("pi's keyboard help format changed");
	}
	return visible;
}

export function applyReasoningUiGuard(): void {
	const footer = FooterComponent.prototype;
	const render = footer.render;
	footer.render = function (width) {
		const host = this as unknown as { session: { state: { model?: object } } };
		const state = {
			...host.session.state,
			model: host.session.state.model ? { ...host.session.state.model, reasoning: false } : undefined,
		};
		const session = new Proxy(host.session, {
			get(target, key, receiver) {
				if (key !== "state") return Reflect.get(target, key, receiver);
				return state;
			},
		});
		const view = Object.create(this);
		Object.defineProperty(view, "session", { value: session });
		return render.call(view, width);
	};
	const settings = SettingsSelectorComponent.prototype;
	const getSettingsList = settings.getSettingsList;
	settings.getSettingsList = function () {
		const list = getSettingsList.call(this);
		const state = list as unknown as {
			items: { id: string }[];
			filteredItems: { id: string }[];
		};
		for (const items of [state.items, state.filteredItems]) {
			for (let index = items.length - 1; index >= 0; index -= 1) {
				if (items[index]!.id === "model-thinking") {
					items.splice(index, 1);
				}
			}
		}
		return list;
	};
	const interactive = InteractiveMode.prototype as unknown as InteractivePrototype;
	if (typeof interactive.cycleThinkingLevel !== "function") {
		throw new Error("pi no longer exposes cycleThinkingLevel, so the thinking level control cannot be hidden");
	}
	interactive.cycleThinkingLevel = () => undefined;
	const showStatus = interactive.showStatus;
	interactive.showStatus = function (message) {
		return showStatus.call(this, withoutThinkingStatus(message));
	};
	const hotkeys = interactive.handleHotkeysCommand;
	interactive.handleHotkeysCommand = function () {
		const container = this.chatContainer;
		const keys = { "Clear the screen": this.getAppKeyDisplay(CLEAR_SCREEN), "Search past prompts": this.getAppKeyDisplay(PROMPT_SEARCH) };
		const view = Object.create(this);
		let guardedHelp = false;
		Object.defineProperty(view, "chatContainer", { value: {
			addChild(child: { text?: string; setText?(text: string): void }) {
				if (child.text?.includes("\n") && child.setText) {
					child.setText(cloudThinkerHotkeys(child.text, keys));
					guardedHelp = true;
				}
				container.addChild(child);
			},
		} });
		hotkeys.call(view);
		if (!guardedHelp) throw new Error("pi's keyboard help rendering seam changed");
	};
}
