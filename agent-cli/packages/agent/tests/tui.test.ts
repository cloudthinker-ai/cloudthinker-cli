import assert from "node:assert/strict";
import test from "node:test";

import { parseArgs } from "@earendil-works/pi-coding-agent";

import { modelScopeArgs } from "../src/models.ts";
import { bundledThemePaths, themeArgs } from "../src/theme.ts";
import { DEFAULT_TUI_MODE, tuiModeArgs } from "../src/tui.ts";

function piArgv(argv: string[]): string[] {
	const themes = themeArgs(bundledThemePaths("/opt/cloudthinker-agent/theme", () => true), argv, undefined);
	return [...themes, ...tuiModeArgs(argv), ...modelScopeArgs(argv), ...argv];
}

test("a run with no TUI flag starts fullscreen", () => {
	assert.deepEqual(tuiModeArgs([]), ["--tui-mode", DEFAULT_TUI_MODE]);
	assert.equal(parseArgs(piArgv([])).tuiMode, "fullscreen");
});

test("the caller's own TUI mode wins over the default", () => {
	assert.deepEqual(tuiModeArgs(["--tui-mode", "regular"]), []);
	const parsed = parseArgs(piArgv(["--tui-mode", "regular"]));
	assert.equal(parsed.tuiMode, "regular");
	assert.deepEqual(parsed.diagnostics, []);
});

test("the injected default leaves the message list untouched", () => {
	const parsed = parseArgs(piArgv(["hello"]));
	assert.equal(parsed.tuiMode, "fullscreen");
	assert.deepEqual(parsed.messages, ["hello"]);
});
