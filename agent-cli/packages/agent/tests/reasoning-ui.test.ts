import assert from "node:assert/strict";
import test from "node:test";
import { stripVTControlCharacters } from "node:util";

import { InteractiveMode } from "@earendil-works/pi-coding-agent";
import { AssistantMessageComponent } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/components/assistant-message.js";
import { FooterComponent } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/components/footer.js";
import { SettingsSelectorComponent } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/components/settings-selector.js";
import { getMarkdownTheme, initTheme } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/theme/theme.js";
import { applyReasoningUiGuard, withoutThinkingHotkeys, withoutThinkingStatus } from "../src/reasoning-ui.ts";

initTheme("light");
applyReasoningUiGuard();

test("the actual footer shows the mode without exposing or changing its reasoning state", () => {
	for (const thinkingLevel of ["off", "low", "medium", "high"]) {
		const session = {
			state: { model: { id: "light", reasoning: true, contextWindow: 200_000 }, thinkingLevel },
			sessionManager: { getEntries: () => [], getCwd: () => "/workspace", getSessionName: () => undefined },
			getContextUsage() {
				assert.equal(this.state, this.state);
				return undefined;
			},
			modelRuntime: { isUsingSubscription: () => false },
		};
		const data = { getGitBranch: () => undefined, getAvailableProviderCount: () => 1, getExtensionStatuses: () => new Map() };
		const component = new FooterComponent(
			session as unknown as ConstructorParameters<typeof FooterComponent>[0],
			data as unknown as ConstructorParameters<typeof FooterComponent>[1],
		);
		const rendered = stripVTControlCharacters(component.render(80).join("\n"));
		assert.match(rendered, /light$/);
		assert.doesNotMatch(rendered, /thinking|medium|high|low|off/);
		assert.equal(session.state.model.reasoning, true);
		assert.equal(session.state.thinkingLevel, thinkingLevel);
	}
});

test("streamed and resumed assistant rendering retains answers but never reveals reasoning", () => {
	const message = {
		role: "assistant", content: [
			{ type: "thinking", thinking: "PRIVATE_REASONING", thinkingSignature: "signed" },
			{ type: "text", text: "Visible answer" },
		], stopReason: "stop",
	} as unknown as Parameters<AssistantMessageComponent["updateContent"]>[0];
	const component = new AssistantMessageComponent(message, false);
	for (const streaming of [true, false]) {
		component.updateContent(message, streaming);
		component.setHideThinkingBlock(false);
		component.invalidate();
		const rendered = stripVTControlCharacters(component.render(80).join("\n"));
		assert.match(rendered, /Visible answer/);
		assert.doesNotMatch(rendered, /PRIVATE_REASONING|Thinking/);
	}
	assert.equal(message.content.length, 2);
	assert.equal(message.content[0]?.type, "thinking");
});

test("thinking keyboard actions cannot change session state or reveal blocks", () => {
	const prototype = InteractiveMode.prototype as unknown as {
		cycleThinkingLevel(): void;
		toggleThinkingBlockVisibility(): void;
	};
	const host = new Proxy({}, { get() { throw new Error("thinking action reached runtime"); } });
	assert.doesNotThrow(() => prototype.cycleThinkingLevel.call(host));
	assert.doesNotThrow(() => prototype.toggleThinkingBlockVisibility.call(host));
});

test("settings search cannot restore either thinking control", () => {
	const config = {
		availableDefaultModels: [], availableThemes: ["light"], modelThinkingLevels: {},
		currentTheme: "light", httpIdleTimeoutMs: 60_000, thinkingLevel: "medium",
	} as unknown as ConstructorParameters<typeof SettingsSelectorComponent>[0];
	const component = new SettingsSelectorComponent(config, {
		onCancel: () => undefined,
	} as unknown as ConstructorParameters<typeof SettingsSelectorComponent>[1]);
	const list = component.getSettingsList();
	list.handleInput("thinking");
	const rendered = stripVTControlCharacters(list.render(80).join("\n"));
	assert.doesNotMatch(rendered, /Hide thinking|Default thinking level/);
	for (let index = 0; index < 8; index += 1) list.handleInput("\u007f");
	list.handleInput("theme");
	assert.match(stripVTControlCharacters(list.render(80).join("\n")), /Theme/);
});

test("built-in keyboard help keeps working controls without advertising thinking", () => {
	const components: { render(width: number): string[] }[] = [];
	const host = {
		getEditorKeyDisplay: () => "Key", getAppKeyDisplay: () => "Key",
		getMarkdownThemeWithSettings: getMarkdownTheme,
		session: { extensionRunner: { getShortcuts: () => new Map() } },
		keybindings: { getEffectiveConfig: () => ({}) },
		chatContainer: { addChild: (child: { render(width: number): string[] }) => components.push(child) },
		ui: { requestRender: () => undefined },
	};
	const prototype = InteractiveMode.prototype as unknown as { handleHotkeysCommand(): void };
	prototype.handleHotkeysCommand.call(host);
	const rendered = stripVTControlCharacters(components.flatMap((component) => component.render(100)).join("\n"));
	assert.match(rendered, /Cycle models/);
	assert.doesNotMatch(rendered, /thinking/i);
});

test("changed upstream thinking presentation fails loudly", () => {
	assert.equal(withoutThinkingStatus("Switched to Light (thinking: medium)"), "Switched to Light");
	assert.equal(withoutThinkingStatus("Ready"), "Ready");
	assert.throws(() => withoutThinkingStatus("Switched to Light thinking: medium"), /format changed/);
	assert.throws(() => withoutThinkingHotkeys("Cycle thinking level\nToggle thinking block visibility"), /format changed/);
	assert.throws(() => withoutThinkingHotkeys("New upstream help format"), /format changed/);
});
