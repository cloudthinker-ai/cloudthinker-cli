import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { InteractiveMode, SettingsManager, loadSkills } from "@earendil-works/pi-coding-agent";
import { Text, type TUI } from "@earendil-works/pi-tui";
import { ToolExecutionComponent } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/components/tool-execution.js";
import { initTheme } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/theme/theme.js";

test("CA-AD-13 pi collapses absent and explicit regular TUI settings", () => {
	assert.equal(SettingsManager.inMemory().getTuiMode(), "regular");
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
