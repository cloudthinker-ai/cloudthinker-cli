import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { InteractiveMode, SettingsManager, loadSkills } from "@earendil-works/pi-coding-agent";
import { type Terminal, Text, type TUI, TuiAltScreen } from "@earendil-works/pi-tui";
import { ToolExecutionComponent } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/components/tool-execution.js";
import { initTheme } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/theme/theme.js";

test("CA-AD-13 pi defaults an absent TUI setting to fullscreen and keeps explicit settings", () => {
	assert.equal(SettingsManager.inMemory().getTuiMode(), "fullscreen");
	assert.equal(SettingsManager.inMemory({ tuiMode: "regular" }).getTuiMode(), "regular");
	assert.equal(SettingsManager.inMemory({ tuiMode: "fullscreen" }).getTuiMode(), "fullscreen");
});

test("CA-AD-13 default skill discovery includes agentDir/skills, not cloud workspace caches", () => {
	const root = mkdtempSync(join(tmpdir(), "ct-skills-contract-"));
	try {
		for (const [directory, name] of [["skills/visible", "visible"], ["cloudthinker/skills/workspace/hidden", "hidden"]]) {
			mkdirSync(join(root, directory!), { recursive: true });
			writeFileSync(join(root, directory!, "SKILL.md"), `---\nname: ${name}\ndescription: Fixture skill\n---\nFixture body\n`);
		}
		const loaded = loadSkills({ cwd: root, agentDir: root, includeDefaults: true, skillPaths: [] });
		assert.ok(loaded.skills.some((skill) => skill.name === "visible"));
		assert.ok(!loaded.skills.some((skill) => skill.name === "hidden"));
	} finally {
		rmSync(root, { recursive: true, force: true });
	}
});

test("CA-AD-13 actual tool expansion reruns renderCall with the expansion state", () => {
	initTheme("dark");
	const states: boolean[] = [];
	const component = new ToolExecutionComponent("fixture", "call-1", {}, {}, {
		renderCall: (_args: unknown, _theme: unknown, context: { expanded: boolean }) => { states.push(context.expanded); return new Text(context.expanded ? "detail" : "summary"); },
	}, { requestRender() {} } as unknown as TUI, process.cwd());
	component.render(80);
	component.setExpanded(true);
	assert.match(component.render(80).join("\n"), /detail/);
	component.setExpanded(false);
	assert.match(component.render(80).join("\n"), /summary/);
	assert.ok(states.includes(true));
	assert.equal(states.at(-1), false);
});

test("CA-AD-13 actual model cycling delegates to the session's registered scope", async () => {
	const calls: string[] = [];
	const prototype = InteractiveMode.prototype as unknown as { cycleModel(direction: string): Promise<void> };
	const host = {
		runtimeHost: { session: { scopedModels: [{ model: { id: "pro" } }], cycleModel: async (direction: string) => { calls.push(direction); return undefined; } } },
		showStatus: () => {},
		showError: (message: string) => { throw new Error(message); },
	};
	Object.setPrototypeOf(host, InteractiveMode.prototype);
	await prototype.cycleModel.call(host, "forward");
	assert.deepEqual(calls, ["forward"]);
});

test("CA-AD-13 pi-tui stop keeps the transcript off the main screen with preserveScreen and restarts for a remote reattach", () => {
	assert.equal(typeof (InteractiveMode.prototype as unknown as { setupKeyHandlers: unknown }).setupKeyHandlers, "function");
	const written: string[] = [];
	const terminal: Terminal = {
		start() {}, stop() {}, drainInput: async () => {}, write: (data: string) => { written.push(data); },
		columns: 40, rows: 10, kittyProtocolActive: false,
		moveBy() {}, hideCursor() {}, showCursor() {}, clearLine() {}, clearFromCursor() {}, clearScreen() {}, setTitle() {}, setProgress() {},
	};
	const ui = new TuiAltScreen(terminal, false, undefined, { mouse: true });
	ui.addChild(new Text("transcript-line"));
	ui.start();
	written.length = 0;
	ui.stop({ preserveScreen: true });
	const preserved = written.join("");
	assert.match(preserved, /\x1b\[\?1049l/);
	assert.doesNotMatch(preserved, /transcript-line/);
	written.length = 0;
	ui.start();
	ui.requestRender(true);
	assert.match(written.join(""), /\x1b\[\?1049h/);
	written.length = 0;
	ui.stop();
	assert.match(written.join(""), /transcript-line/);
});

test("CA-AD-13 Ctrl+D, Ctrl+C twice, and /quit leave through shutdown, pi still defines handleCtrlZ, and pi's selector picks its first option on Enter and nothing on Esc", async () => {
	initTheme("dark");
	const prototype = InteractiveMode.prototype as unknown as {
		shutdown: unknown;
		showStatus: unknown;
		handleCtrlZ: unknown;
		handleCtrlC(): void;
		handleCtrlD(): void;
		setupEditorSubmitHandler(): void;
		showExtensionSelector(title: string, options: string[]): Promise<string | undefined>;
	};
	assert.equal(typeof prototype.shutdown, "function");
	assert.equal(typeof prototype.showStatus, "function");
	assert.equal(typeof prototype.handleCtrlZ, "function");
	const exits: unknown[] = [];
	let shown: { handleInput(data: string): void } | undefined;
	const host = {
		lastSigintTime: Date.now(),
		defaultEditor: {} as { onSubmit?: (text: string) => Promise<void> },
		editor: { setText() {} },
		editorContainer: { clear() {}, addChild(child: { handleInput(data: string): void }) { shown = child; } },
		ui: { setFocus() {}, requestRender() {} },
		shutdown: async (options?: unknown) => { exits.push(options); },
	};
	Object.setPrototypeOf(host, InteractiveMode.prototype);
	prototype.handleCtrlD.call(host);
	prototype.handleCtrlC.call(host);
	prototype.setupEditorSubmitHandler.call(host);
	await host.defaultEditor.onSubmit?.("/quit");
	assert.deepEqual(exits, [undefined, undefined, undefined]);

	const picked = prototype.showExtensionSelector.call(host, "Leave?", ["Detach", "Quit"]);
	assert.equal((host as { extensionSelector?: unknown }).extensionSelector, shown);
	shown?.handleInput("\r");
	assert.equal((host as { extensionSelector?: unknown }).extensionSelector, undefined);
	assert.equal(await picked, "Detach");
	const cancelled = prototype.showExtensionSelector.call(host, "Leave?", ["Detach", "Quit"]);
	shown?.handleInput("\x1b");
	assert.equal(await cancelled, undefined);
});
