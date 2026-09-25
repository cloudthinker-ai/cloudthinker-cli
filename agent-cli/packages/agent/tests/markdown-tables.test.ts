import assert from "node:assert/strict";
import test from "node:test";
import { stripVTControlCharacters } from "node:util";

import { Markdown, visibleWidth } from "@earendil-works/pi-tui";
import { getMarkdownTheme, initTheme } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/theme/theme.js";

initTheme("dark");

const comparison = `| Month | MRs reviewed | September difference |
|---|---:|---:|
| June MTD | 243 | +25.1% |
| July MTD | 190 | +60.0% |
| August MTD | 314 | -3.2% |
| September MTD | 304 | — |`;

test("chat tables have quiet separators and aligned numeric values", () => {
	const lines = new Markdown(comparison, 0, 0, getMarkdownTheme()).render(90)
		.map(stripVTControlCharacters).map((line) => line.trimEnd());
	assert.equal(lines.length, 9);
	assert.match(lines[0]!, /^Month\s+MRs reviewed\s+September difference$/);
	assert.match(lines[1]!, /^─+   ─+   ─+$/);
	assert.match(lines[2]!, /^June MTD\s+243\s+\+25\.1%$/);
	assert.match(lines[8]!, /^September MTD\s+304\s+—$/);
	assert.ok(lines.every((line) => !/[┌┐└┘│├┼┤]/.test(line)));
	assert.equal(lines[2]!.indexOf("243") + 3, lines[4]!.indexOf("190") + 3);
});

test("chat tables fit a narrow terminal while keeping adjacent prose", () => {
	const markdown = `Before\n\n${comparison}\n\nAfter`;
	const lines = new Markdown(markdown, 0, 0, getMarkdownTheme()).render(45)
		.map(stripVTControlCharacters).map((line) => line.trimEnd());
	assert.ok(lines.every((line) => visibleWidth(line) <= 45));
	assert.ok(lines.some((line) => line.includes("+25.1%")));
	assert.ok(lines.some((line) => line.includes("September")));
	assert.equal(lines[0], "Before");
	assert.equal(lines.at(-1), "After");
});

test("numeric columns align without Markdown alignment markers", () => {
	const source = `| Workspace | Jun MTD | Sep vs Aug |
|---|---|---|
| Alpha | 9 | -14.1% |
| Longer name | 113 | +100.0% |`;
	const lines = new Markdown(source, 0, 0, getMarkdownTheme()).render(60)
		.map(stripVTControlCharacters).map((line) => line.trimEnd());
	assert.equal(lines[2]!.indexOf("9") + 1, lines[4]!.indexOf("113") + 3);
	assert.equal(lines[2]!.indexOf("-14.1%") + 6, lines[4]!.indexOf("+100.0%") + 7);
});
