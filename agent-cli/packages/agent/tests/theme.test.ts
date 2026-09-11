import assert from "node:assert/strict";
import test from "node:test";

import { DEFAULT_THEME, bundledThemePaths, themeArgs } from "../src/theme.ts";

const paths = bundledThemePaths("/opt/cloudthinker-agent/theme", () => true);

test("both themes are looked for beside the binary and skipped when absent", () => {
	assert.deepEqual(paths, [
		"/opt/cloudthinker-agent/theme/cloudthinker-dark.json",
		"/opt/cloudthinker-agent/theme/cloudthinker-light.json",
	]);
	assert.deepEqual(bundledThemePaths("/opt/cloudthinker-agent/theme", () => false), []);
});

test("the bundled themes register with an automatic default", () => {
	assert.deepEqual(themeArgs(paths, [], undefined), [
		"--theme",
		"/opt/cloudthinker-agent/theme/cloudthinker-dark.json",
		"--theme",
		"/opt/cloudthinker-agent/theme/cloudthinker-light.json",
		"--use-theme",
		DEFAULT_THEME,
	]);
});

test("a saved theme keeps its place while the bundled themes stay selectable", () => {
	const args = themeArgs(paths, [], "light");
	assert.ok(args.includes("--theme"));
	assert.ok(!args.includes("--use-theme"));
});

test("the caller's own theme choice is never overridden", () => {
	const args = themeArgs(paths, ["--use-theme", "dark"], undefined);
	assert.equal(args.filter((arg) => arg === "--use-theme").length, 0);
	assert.deepEqual(themeArgs(paths, ["--no-themes"], undefined), []);
});

test("a build with no bundled themes adds no theme flags at all", () => {
	assert.deepEqual(themeArgs([], [], undefined), []);
});
