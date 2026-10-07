import assert from "node:assert/strict";
import test from "node:test";
import { stripVTControlCharacters } from "node:util";

import { Editor, type TUI, type TuiMouseEvent } from "@earendil-works/pi-tui";

import { getEditorTheme, initTheme } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/theme/theme.js";
import { applyEditorSelection, paintColumns } from "../src/editor-selection.ts";

initTheme("dark");
applyEditorSelection();

const BACKSPACE = "\x7f";
const DELETE = "\x1b[3~";
const RIGHT = "\x1b[C";

function setup(text: string) {
	const copied: string[] = [];
	const tui = {
		requestRender: () => {},
		terminal: { rows: 40, columns: 40 },
		getCopyOnSelect: () => true,
		copyTextToClipboard: async (value: string) => (copied.push(value), true),
	} as unknown as TUI;
	const editor = new Editor(tui, getEditorTheme());
	editor.setText(text);
	editor.render(40);
	const mouse = (type: TuiMouseEvent["type"], x: number, y: number, clickCount?: number) =>
		editor.handleMouse({ type, button: "left", x, y, screenX: x, screenY: y, width: 40, height: 10, shift: false, alt: false, ctrl: false, ...(clickCount ? { clickCount } : {}) });
	const click = (x: number, y: number, clickCount: number) => {
		mouse("press", x, y);
		mouse("release", x, y);
		return mouse("click", x, y, clickCount);
	};
	const drag = (from: [number, number], to: [number, number]) => {
		mouse("press", ...from);
		mouse("drag", ...to);
		mouse("release", ...to);
		editor.render(40);
	};
	const painted = () => editor.render(40).slice(1, -1).some((line) => line.includes("\x1b[27m"));
	return { editor, copied, click, drag, painted };
}

test("triple-click selects the whole prompt and Backspace clears it", () => {
	const { editor, click, painted } = setup("first line\nsecond line\nthird");
	assert.deepEqual(click(3, 2, 3), { handled: true, focus: true });
	assert.ok(painted(), "the selection is painted");
	editor.handleInput(BACKSPACE);
	assert.equal(editor.getText(), "");
	(editor as unknown as { undo(): void }).undo();
	assert.equal(editor.getText(), "first line\nsecond line\nthird", "undo brings the prompt back");
});

test("dragging selects within and across lines, copies, and typing replaces it", () => {
	const { editor, copied, drag } = setup("hello world\nsecond line");
	drag([0, 1], [4, 1]);
	assert.deepEqual(copied, ["hello"], "release copies like the screen selection did");
	editor.handleInput("X");
	assert.equal(editor.getText(), "X world\nsecond line");

	drag([2, 1], [5, 2]);
	editor.handleInput(DELETE);
	assert.equal(editor.getText(), "X  line", "a drag over a line break joins the lines");

	drag([4, 1], [0, 1]);
	editor.handleInput(BACKSPACE);
	assert.equal(editor.getText(), "ne", "a backward drag includes the anchor cell");
});

test("double-click selects a word, and a plain click or a cursor key drops the selection", () => {
	const { editor, click, painted } = setup("check ./src/app.ts now");
	click(9, 1, 2);
	editor.handleInput(DELETE);
	assert.equal(editor.getText(), "check  now", "a path stays one word");

	click(1, 1, 2);
	click(1, 1, 1);
	assert.ok(!painted(), "a single click clears the selection");
	editor.handleInput(BACKSPACE);
	assert.equal(editor.getText(), "heck  now", "Backspace after a click deletes one character");

	click(1, 1, 3);
	editor.handleInput(RIGHT);
	editor.handleInput(BACKSPACE);
	assert.equal(editor.getText(), "heck  no", "a cursor key drops the selection without editing");
});

test("a selection goes stale once the text changes under it", () => {
	const { editor, click, painted } = setup("draft");
	click(0, 1, 3);
	editor.setText("new text");
	assert.ok(!painted());
	editor.handleInput(BACKSPACE);
	assert.equal(editor.getText(), "new tex");
});

test("paintColumns reverses only the chosen columns and survives inner resets", () => {
	assert.equal(paintColumns("abcdef", 1, 3), "a\x1b[7mbc\x1b[27mdef");
	assert.equal(paintColumns("a\x1b[0mbc", 0, 3), "\x1b[7ma\x1b[0m\x1b[7mbc\x1b[27m");
	assert.equal(paintColumns("日本語", 2, 4), "日\x1b[7m本\x1b[27m語");
});
