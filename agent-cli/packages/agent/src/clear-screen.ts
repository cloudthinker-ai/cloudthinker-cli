import { InteractiveMode } from "@earendil-works/pi-coding-agent";

import { KEYBINDINGS } from "../node_modules/@earendil-works/pi-coding-agent/dist/core/keybindings.js";

export const CLEAR_SCREEN = "app.screen.clear";
export const CLEAR_SCREEN_KEY = "ctrl+l";
export const MODEL_SELECT = "app.model.select";
export const CLEAR_SCREEN_BUSY = "Ctrl+L is disabled while a task is in progress";

interface KeybindingDefinition {
	defaultKeys: string | string[];
	description?: string;
}

export interface ClearScreenHost {
	chatContainer: { clear(): void };
	session: { isStreaming: boolean; isBashRunning: boolean; isCompacting?: boolean };
	showStatus(message: string): void;
	ui: { requestRender(force?: boolean): void };
}

interface ClearScreenPrototype {
	setupKeyHandlers(this: ClearScreenHost & { defaultEditor: { onAction(action: string, handler: () => void): void } }): void;
}

export function clearScreen(host: ClearScreenHost): void {
	if (host.session.isStreaming || host.session.isBashRunning || host.session.isCompacting) {
		host.showStatus(CLEAR_SCREEN_BUSY);
		return;
	}
	host.chatContainer.clear();
	host.ui.requestRender(true);
}

export function applyClearScreenKey(): void {
	const definitions = KEYBINDINGS as unknown as Record<string, KeybindingDefinition>;
	const modelSelect = definitions[MODEL_SELECT];
	if (modelSelect?.defaultKeys !== CLEAR_SCREEN_KEY || CLEAR_SCREEN in definitions) {
		throw new Error(`pi no longer binds ${CLEAR_SCREEN_KEY} to ${MODEL_SELECT}, so Ctrl+L cannot clear the screen`);
	}
	const prototype = InteractiveMode.prototype as unknown as ClearScreenPrototype;
	const setupKeyHandlers = prototype.setupKeyHandlers;
	if (typeof setupKeyHandlers !== "function") {
		throw new Error("pi's InteractiveMode no longer defines setupKeyHandlers, so Ctrl+L cannot clear the screen");
	}
	modelSelect.defaultKeys = [];
	definitions[CLEAR_SCREEN] = { defaultKeys: CLEAR_SCREEN_KEY, description: "Clear the screen" };
	prototype.setupKeyHandlers = function () {
		setupKeyHandlers.call(this);
		this.defaultEditor.onAction(CLEAR_SCREEN, () => clearScreen(this));
	};
}
