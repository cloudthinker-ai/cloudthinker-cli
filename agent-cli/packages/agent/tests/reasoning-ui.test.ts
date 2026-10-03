import assert from "node:assert/strict";
import test from "node:test";
import { stripVTControlCharacters } from "node:util";

import { InteractiveMode, SettingsManager } from "@earendil-works/pi-coding-agent";
import { AssistantMessageComponent } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/components/assistant-message.js";
import { FooterComponent } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/components/footer.js";
import { WorkingStatusIndicator } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/components/status-indicator.js";
import { SettingsSelectorComponent } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/components/settings-selector.js";
import { getMarkdownTheme, initTheme } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/theme/theme.js";
import { applyReasoningUiGuard, cloudThinkerHotkeys, withoutThinkingStatus } from "../src/reasoning-ui.ts";
import { applyThinkingUi, formatThoughtTime } from "../src/thinking-ui.ts";

initTheme("light");
applyReasoningUiGuard();
applyThinkingUi();

test("the actual footer shows the mode without exposing or changing its reasoning state", () => {
	for (const thinkingLevel of ["off", "low", "medium", "high"]) {
		const session = {
			state: { model: { id: "light", reasoning: true, contextWindow: 200_000 }, thinkingLevel },
			sessionManager: { getEntries: () => [], getEntryCount: () => 0, getSessionId: () => "footer", getLeafId: () => null, getCwd: () => "/workspace", getSessionName: () => undefined },
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

test("thinking leaves the chat: it shows live on the working line and a finished thought takes no rows", () => {
	assert.equal(SettingsManager.inMemory().getHideThinkingBlock(), true);
	assert.equal(SettingsManager.inMemory({ hideThinkingBlock: false }).getHideThinkingBlock(), false);
	const thinking = { type: "thinking", thinking: "step one\nstep two", thinkingSignature: "signed" };
	const message = (...content: object[]) =>
		({ role: "assistant", content, stopReason: "stop" }) as unknown as Parameters<AssistantMessageComponent["updateContent"]>[0];
	const screen = (component: AssistantMessageComponent) => stripVTControlCharacters(component.render(80).join("\n"));
	const working = new WorkingStatusIndicator({ requestRender() {} } as never, "Working (esc to interrupt)");
	const status = () => stripVTControlCharacters(working.render(80).join("\n")).trim();
	try {
		const live = new AssistantMessageComponent(undefined, true);
		live.updateContent(message(thinking), true);
		assert.deepEqual(live.render(80), []);
		working.invalidate();
		assert.match(status(), /Thinking 1s \(esc to interrupt\)$/);
		live.updateContent(message(thinking, { type: "text", text: "Visible answer" }), true);
		working.invalidate();
		assert.match(status(), /Working \(esc to interrupt\)$/);
		assert.equal(screen(live).trim(), "Visible answer");

		const toolStep = new AssistantMessageComponent(message(thinking, { type: "toolCall", id: "t-1", name: "bash", arguments: {} }), true);
		assert.deepEqual(toolStep.render(80), []);

		const resumed = new AssistantMessageComponent(message({ type: "text", text: "Before" }, thinking, { type: "text", text: "Visible answer" }), true);
		assert.doesNotMatch(screen(resumed), /step|Thought/);
		resumed.setHideThinkingBlock(false);
		assert.match(screen(resumed), /Before[\s\S]*step one[\s\S]*Visible answer/);
		resumed.setHideThinkingBlock(true);
		assert.doesNotMatch(screen(resumed), /step one/);
	} finally {
		working.dispose();
	}
	assert.equal(formatThoughtTime(65_000), "1m 5s");
	assert.equal(formatThoughtTime(120_000), "2m");
});

test("the thinking level key cannot change session state", () => {
	const prototype = InteractiveMode.prototype as unknown as { cycleThinkingLevel(): void };
	const host = new Proxy({}, { get() { throw new Error("thinking action reached runtime"); } });
	assert.doesNotThrow(() => prototype.cycleThinkingLevel.call(host));
});

test("settings offer hiding thinking but never the thinking level", () => {
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
	assert.match(rendered, /Hide thinking/);
	assert.doesNotMatch(rendered, /Default thinking level/);
	for (let index = 0; index < 8; index += 1) list.handleInput("\u007f");
	list.handleInput("theme");
	assert.match(stripVTControlCharacters(list.render(80).join("\n")), /Theme/);
});

test("built-in keyboard help keeps working controls without advertising thinking", () => {
	const components: { render(width: number): string[] }[] = [];
	const host = {
		getEditorKeyDisplay: () => "Key", getAppKeyDisplay: (action: string) => ({ "app.screen.clear": "Ctrl+L", "app.prompt.search": "Ctrl+R" })[action] ?? "Key",
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
	assert.match(rendered, /Open the transcript/);
	assert.match(rendered, /Ctrl\+L\s+Clear the screen/);
	assert.match(rendered, /Ctrl\+R\s+Search past prompts/);
	assert.doesNotMatch(rendered, /model selector/);
	assert.match(rendered, /Show or hide thinking/);
	assert.doesNotMatch(rendered, /thinking level|Toggle tool output/i);
});

test("changed upstream thinking presentation fails loudly", () => {
	assert.equal(withoutThinkingStatus("Switched to Light (thinking: medium)"), "Switched to Light");
	assert.equal(withoutThinkingStatus("Ready"), "Ready");
	assert.throws(() => withoutThinkingStatus("Switched to Light thinking: medium"), /format changed/);
	assert.throws(() => cloudThinkerHotkeys("Cycle thinking level\nToggle thinking block visibility"), /format changed/);
	assert.throws(() => cloudThinkerHotkeys("New upstream help format"), /format changed/);
});
