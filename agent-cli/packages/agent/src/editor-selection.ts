import { Editor, getKeybindings, isKeyRelease, visibleWidth, type TuiMouseEvent, type TuiMouseEventResult } from "@earendil-works/pi-tui";

import { decodePrintableKey } from "../node_modules/@earendil-works/pi-tui/dist/keys.js";

// Fullscreen mouse selection inside the prompt box: drag, double-click a word, or
// triple-click the whole prompt, then Backspace or Delete removes it and typing
// replaces it. pi leaves editor drags to the screen-level copy selection, which
// cannot edit, so the editor captures its own text rows and still copies on release.

interface Position {
	line: number;
	col: number;
}

interface Cell {
	line: number;
	start: number;
	end: number;
}

interface Selection {
	start: Position;
	end: Position;
	text: string;
}

interface VisualLine {
	logicalLine: number;
	startCol: number;
	length: number;
}

interface EditorLike {
	state: { lines: string[]; cursorLine: number; cursorCol: number };
	scrollOffset: number;
	renderedVisibleLineCount: number;
	lastWidth: number;
	paddingX: number;
	lastAction: unknown;
	autocompleteState: unknown;
	tui: { getCopyOnSelect?(): boolean; copyTextToClipboard?(text: string): Promise<boolean> };
	onChange?: (text: string) => void;
	buildVisualLineMap(width: number): VisualLine[];
	setCursorCol(col: number): void;
	pushUndoSnapshot(): void;
	exitHistoryBrowsing(): void;
	updateAutocomplete(): void;
	expandPasteMarkers(text: string): string;
	getText(): string;
	render(width: number): string[];
	handleInput(data: string): void;
	handleMouse(event: TuiMouseEvent): TuiMouseEventResult | undefined;
}

const SELECT_ON = "\x1b[7m";
const SELECT_OFF = "\x1b[27m";
const ESCAPE = /\x1b\[[0-9;?]*[A-Za-z]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b_[^\x07\x1b]*(?:\x07|\x1b\\)/y;
const SGR = /^\x1b\[[0-9;]*m$/;
const graphemes = new Intl.Segmenter(undefined, { granularity: "grapheme" });

const selections = new WeakMap<object, Selection>();
const drags = new WeakMap<object, { anchor: Cell; moved: boolean }>();

function compare(left: Position, right: Position): number {
	return left.line - right.line || left.col - right.col;
}

function snapshot(editor: EditorLike): string {
	return editor.state.lines.join("\n");
}

/** The live selection, dropped once the text changed under it or it is empty. */
export function activeSelection(editor: EditorLike): Selection | undefined {
	const selection = selections.get(editor);
	if (!selection) return undefined;
	if (selection.text !== snapshot(editor) || compare(selection.start, selection.end) >= 0) {
		selections.delete(editor);
		return undefined;
	}
	return selection;
}

function select(editor: EditorLike, start: Position, end: Position, cursor: Position): void {
	selections.set(editor, { start, end, text: snapshot(editor) });
	editor.state.cursorLine = cursor.line;
	editor.setCursorCol(cursor.col);
	editor.lastAction = null;
}

export function selectedText(editor: EditorLike): string | undefined {
	const selection = activeSelection(editor);
	if (!selection) return undefined;
	const { start, end } = selection;
	const lines = editor.state.lines;
	if (start.line === end.line) return lines[start.line]!.slice(start.col, end.col);
	return [lines[start.line]!.slice(start.col), ...lines.slice(start.line + 1, end.line), lines[end.line]!.slice(0, end.col)].join("\n");
}

function deleteSelection(editor: EditorLike, selection: Selection): void {
	const { start, end } = selection;
	selections.delete(editor);
	editor.exitHistoryBrowsing();
	editor.pushUndoSnapshot();
	const lines = editor.state.lines;
	lines.splice(start.line, end.line - start.line + 1, lines[start.line]!.slice(0, start.col) + lines[end.line]!.slice(end.col));
	editor.state.cursorLine = start.line;
	editor.setCursorCol(start.col);
	editor.lastAction = null;
	editor.onChange?.(editor.getText());
	if (editor.autocompleteState) editor.updateAutocomplete();
}

function paddingOf(editor: EditorLike, width: number): number {
	return Math.min(editor.paddingX, Math.max(0, Math.floor((width - 1) / 2)));
}

/** The grapheme under a mouse cell; rows past the text clamp to its first or last visible row. */
function cellAt(editor: EditorLike, event: TuiMouseEvent): Cell | undefined {
	const rows = editor.renderedVisibleLineCount;
	if (rows < 1) return undefined;
	const visualLines = editor.buildVisualLineMap(editor.lastWidth);
	const row = Math.min(Math.max(event.y, 1), rows);
	const visual = visualLines[editor.scrollOffset + row - 1];
	if (!visual) return undefined;
	const text = editor.state.lines[visual.logicalLine] ?? "";
	const chunk = text.slice(visual.startCol, visual.startCol + visual.length);
	const target = event.y < 1 ? -1 : event.y > rows ? Number.POSITIVE_INFINITY : event.x - paddingOf(editor, event.width);
	if (target < 0) return { line: visual.logicalLine, start: visual.startCol, end: visual.startCol };
	let column = 0;
	for (const { segment, index } of graphemes.segment(chunk)) {
		const next = column + visibleWidth(segment);
		if (target < next) return { line: visual.logicalLine, start: visual.startCol + index, end: visual.startCol + index + segment.length };
		column = next;
	}
	const end = visual.startCol + chunk.length;
	return { line: visual.logicalLine, start: end, end };
}

function selectWord(editor: EditorLike, cell: Cell): void {
	const text = editor.state.lines[cell.line] ?? "";
	if (cell.start >= text.length) return;
	const space = /\s/.test(text[cell.start]!);
	const same = (char: string) => /\s/.test(char) === space;
	let start = cell.start;
	let end = cell.start;
	while (start > 0 && same(text[start - 1]!)) start--;
	while (end < text.length && same(text[end]!)) end++;
	select(editor, { line: cell.line, col: start }, { line: cell.line, col: end }, { line: cell.line, col: end });
}

function selectAll(editor: EditorLike): void {
	const last = editor.state.lines.length - 1;
	const end = { line: last, col: editor.state.lines[last]!.length };
	select(editor, { line: 0, col: 0 }, end, end);
}

function dragTo(editor: EditorLike, anchor: Cell, cell: Cell): void {
	const forward = compare({ line: cell.line, col: cell.end }, { line: anchor.line, col: anchor.start }) > 0;
	if (forward) {
		const end = { line: cell.line, col: cell.end };
		select(editor, { line: anchor.line, col: anchor.start }, end, end);
		return;
	}
	const start = { line: cell.line, col: cell.start };
	select(editor, start, { line: anchor.line, col: anchor.end }, start);
}

/** Paint visible columns [from, to) in reverse video, re-opening it after any inner style reset. */
export function paintColumns(line: string, from: number, to: number): string {
	let out = "";
	let column = 0;
	let open = false;
	let plain = "";
	const flush = () => {
		for (const { segment } of graphemes.segment(plain)) {
			if (!open && column >= from && column < to) {
				out += SELECT_ON;
				open = true;
			}
			out += segment;
			column += visibleWidth(segment);
			if (open && column >= to) {
				out += SELECT_OFF;
				open = false;
			}
		}
		plain = "";
	};
	for (let index = 0; index < line.length; ) {
		ESCAPE.lastIndex = index;
		const escape = ESCAPE.exec(line);
		if (!escape) {
			plain += line[index];
			index += 1;
			continue;
		}
		flush();
		out += escape[0];
		if (open && SGR.test(escape[0])) out += SELECT_ON;
		index += escape[0].length;
	}
	flush();
	return open ? out + SELECT_OFF : out;
}

function paintSelection(editor: EditorLike, rendered: string[], width: number): string[] {
	const selection = activeSelection(editor);
	if (!selection) return rendered;
	const { start, end } = selection;
	const visualLines = editor.buildVisualLineMap(editor.lastWidth);
	const padding = paddingOf(editor, width);
	for (let row = 1; row <= editor.renderedVisibleLineCount && row < rendered.length; row++) {
		const index = editor.scrollOffset + row - 1;
		const visual = visualLines[index];
		if (!visual || visual.logicalLine < start.line || visual.logicalLine > end.line) continue;
		const text = editor.state.lines[visual.logicalLine] ?? "";
		const segmentEnd = visual.startCol + visual.length;
		const from = Math.max(visual.logicalLine === start.line ? start.col : 0, visual.startCol);
		const to = Math.min(visual.logicalLine === end.line ? end.col : text.length, segmentEnd);
		if (from > segmentEnd) continue;
		const lastSegment = visualLines[index + 1]?.logicalLine !== visual.logicalLine;
		// A selected line break shows as one painted cell after the line's text.
		const lineBreak = lastSegment && visual.logicalLine < end.line ? 1 : 0;
		const left = visibleWidth(text.slice(visual.startCol, from));
		const right = (to > from ? visibleWidth(text.slice(visual.startCol, to)) : left) + lineBreak;
		if (right > left) rendered[row] = paintColumns(rendered[row]!, padding + left, padding + right);
	}
	return rendered;
}

function isTyping(data: string): boolean {
	if (data.includes("\x1b[200~") || decodePrintableKey(data) !== undefined) return true;
	return data.length > 0 && !data.startsWith("\x1b") && [...data].every((char) => char >= " " && char !== "\x7f");
}

export function applyEditorSelection(): void {
	const editor = Editor.prototype as unknown as EditorLike;
	const { render, handleInput, handleMouse } = editor;
	if (typeof render !== "function" || typeof handleInput !== "function" || typeof handleMouse !== "function" || typeof editor.buildVisualLineMap !== "function") {
		throw new Error("pi's editor changed, so prompt text cannot be selected with the mouse");
	}
	editor.render = function (width) {
		return paintSelection(this, render.call(this, width), width);
	};
	editor.handleInput = function (data) {
		const selection = activeSelection(this);
		if (!selection || isKeyRelease(data)) return handleInput.call(this, data);
		const keys = getKeybindings();
		if (keys.matches(data, "tui.editor.deleteCharBackward") || keys.matches(data, "tui.editor.deleteCharForward")) {
			deleteSelection(this, selection);
			return;
		}
		if (isTyping(data)) deleteSelection(this, selection);
		else selections.delete(this);
		handleInput.call(this, data);
	};
	editor.handleMouse = function (event) {
		const rows = this.renderedVisibleLineCount;
		const drag = drags.get(this);
		if (event.type === "press") {
			if (event.button !== "left" || event.y < 1 || event.y > rows) return handleMouse.call(this, event);
			const anchor = cellAt(this, event);
			if (!anchor) return handleMouse.call(this, event);
			selections.delete(this);
			drags.set(this, { anchor, moved: false });
			return { handled: true, capture: true, focus: true };
		}
		if (event.type === "drag" && drag) {
			const cell = cellAt(this, event);
			if (cell) {
				drag.moved = true;
				dragTo(this, drag.anchor, cell);
			}
			return { handled: true };
		}
		if (event.type === "release" && drag) {
			drags.delete(this);
			const text = drag.moved ? selectedText(this) : undefined;
			if (text && this.tui.getCopyOnSelect?.()) void this.tui.copyTextToClipboard?.(this.expandPasteMarkers(text));
			return { handled: true, render: true };
		}
		if (event.type === "click" && event.button === "left" && event.y >= 1 && event.y <= rows) {
			const cell = cellAt(this, event);
			if (cell && event.clickCount === 2) {
				selectWord(this, cell);
				return { handled: true, focus: true };
			}
			if (cell && event.clickCount === 3) {
				selectAll(this);
				return { handled: true, focus: true };
			}
			selections.delete(this);
		}
		return handleMouse.call(this, event);
	};
}
