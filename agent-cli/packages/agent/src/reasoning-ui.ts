import { InteractiveMode } from "@earendil-works/pi-coding-agent";

import { AssistantMessageComponent } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/components/assistant-message.js";
import { FooterComponent } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/components/footer.js";
import { SettingsSelectorComponent } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/components/settings-selector.js";

interface InteractivePrototype {
	cycleThinkingLevel(): void;
	toggleThinkingBlockVisibility(): void;
	showStatus(message: string): void;
	handleHotkeysCommand(): void;
	chatContainer: { addChild(child: { text?: string; setText?(text: string): void }): void };
}

export function withoutThinkingStatus(message: string): string {
	const visible = message.replace(/ \(thinking: [^)]+\)$/, "");
	if (/\bthinking\s*:/i.test(visible)) {
		throw new Error("pi's thinking status format changed");
	}
	return visible;
}

export function withoutThinkingHotkeys(text: string): string {
	const lines = text.split("\n");
	const visible = lines.filter((line) =>
		!/^\|.*\| (Cycle thinking level|Toggle thinking block visibility) \|$/.test(line),
	);
	if (lines.length - visible.length !== 2 || /\bthinking\b/i.test(visible.join("\n"))) {
		throw new Error("pi's thinking keyboard help format changed");
	}
	return visible.join("\n");
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
	const assistant = AssistantMessageComponent.prototype;
	const updateContent = assistant.updateContent;
	assistant.updateContent = function (message, isStreaming) {
		return updateContent.call(this, {
			...message,
			content: message.content.filter((block) => block.type !== "thinking"),
		}, isStreaming);
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
				if (["hide-thinking", "model-thinking"].includes(items[index]!.id)) {
					items.splice(index, 1);
				}
			}
		}
		return list;
	};
	const interactive = InteractiveMode.prototype as unknown as InteractivePrototype;
	for (const method of ["cycleThinkingLevel", "toggleThinkingBlockVisibility"] as const) {
		if (typeof interactive[method] !== "function") {
			throw new Error(`pi no longer exposes ${method}, so thinking controls cannot be hidden`);
		}
		interactive[method] = () => undefined;
	}
	const showStatus = interactive.showStatus;
	interactive.showStatus = function (message) {
		return showStatus.call(this, withoutThinkingStatus(message));
	};
	const hotkeys = interactive.handleHotkeysCommand;
	interactive.handleHotkeysCommand = function () {
		const container = this.chatContainer;
		const view = Object.create(this);
		let guardedHelp = false;
		Object.defineProperty(view, "chatContainer", { value: {
			addChild(child: { text?: string; setText?(text: string): void }) {
				if (child.text?.includes("\n") && child.setText) {
					child.setText(withoutThinkingHotkeys(child.text));
					guardedHelp = true;
				}
				container.addChild(child);
			},
		} });
		hotkeys.call(view);
		if (!guardedHelp) throw new Error("pi's keyboard help rendering seam changed");
	};
}
