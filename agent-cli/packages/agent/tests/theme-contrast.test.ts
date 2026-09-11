import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import test from "node:test";

import {
	detectTerminalThemeForAuto,
	getMarkdownTheme,
	loadThemeFromPath,
	resolveThemeSetting,
	setThemeInstance,
} from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/theme/theme.js";
import { bundledThemePaths, themeArgs } from "../src/theme.ts";

function luminance(rgb: number[]): number {
	return rgb.reduce((sum, channel, index) => {
		const value = channel / 255;
		const linear = value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
		return sum + linear * [0.2126, 0.7152, 0.0722][index]!;
	}, 0);
}

for (const background of [255, 18]) {
	test(`default Markdown colors stay readable on terminal background ${background}`, async () => {
		const terminalTheme = await detectTerminalThemeForAuto({
			ui: {
				queryTerminalBackgroundColor: async () => ({ r: background, g: background, b: background }),
			},
			timeoutMs: 100,
			env: {},
		});
		const directory = fileURLToPath(new URL("../themes/", import.meta.url));
		const args = themeArgs(bundledThemePaths(directory), [], undefined);
		const selected = resolveThemeSetting(args[args.indexOf("--use-theme") + 1], terminalTheme);
		assert.ok(selected);
		setThemeInstance(loadThemeFromPath(`${directory}${selected}.json`, "truecolor"));
		const markdown = getMarkdownTheme();
		for (const rendered of [markdown.heading(markdown.bold("Heading")), markdown.code("code"), markdown.link("link")]) {
			const match = /\x1b\[38;2;(\d+);(\d+);(\d+)m/.exec(rendered);
			assert.ok(match);
			const foreground = luminance(match.slice(1).map(Number));
			const surface = luminance([background, background, background]);
			const contrast = (Math.max(foreground, surface) + 0.05) / (Math.min(foreground, surface) + 0.05);
			assert.ok(contrast >= 4.5, `${selected}: contrast ${contrast.toFixed(2)} on ${background}`);
		}
	});
}
