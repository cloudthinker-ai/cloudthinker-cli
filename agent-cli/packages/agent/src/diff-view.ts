import { stripTerminalSequences, type Component } from "@earendil-works/pi-tui";
import { getLanguageFromPath, highlightCode } from "@earendil-works/pi-coding-agent";
import * as Diff from "diff";

import { sanitizeTerminalText } from "@cloudthinker/cloud/src/awareness.ts";

import { supportsLanguage } from "../node_modules/@earendil-works/pi-coding-agent/dist/utils/syntax-highlight.js";
import { ansiCells, cellsWidth, chunkCells, toCells, type Cell } from "./wrap.ts";

import type { DiffStyle } from "./verbosity.ts";

export const CONTEXT_LINES = 3;
export const PREVIEW_ROWS = 12;
export const SPLIT_MIN_PANE = 60;
const FORCED_SPLIT_MIN_PANE = 20;
const DIVIDER_WIDTH = 3;
const MARK_LIMIT = 0.7;

export interface DiffRow {
	kind: "ctx" | "add" | "del";
	oldNo?: number;
	newNo?: number;
	text: string;
	marks?: boolean[];
	ansi?: string;
	cells?: Cell[];
}

export type Hunk = DiffRow[];

type Block = { ctx: DiffRow } | { dels: DiffRow[]; adds: DiffRow[] };

function cleanText(text: string): string {
	return sanitizeTerminalText(text.replace(/\t/g, "   "));
}

export function parsePiDiff(diff: string): Hunk[] {
	const sections: DiffRow[][] = [[]];
	let delta = 0;
	for (const line of diff.split("\n")) {
		const match = /^([+\- ]) *(\d*) (.*)$/s.exec(line);
		if (!match) continue;
		const [, prefix, digits, raw] = match as unknown as [string, string, string, string];
		const text = cleanText(raw);
		if (digits === "") {
			sections.push([]);
			continue;
		}
		const number = Number(digits);
		const rows = sections[sections.length - 1]!;
		if (prefix === "+") {
			rows.push({ kind: "add", newNo: number, text });
			delta++;
		} else if (prefix === "-") {
			rows.push({ kind: "del", oldNo: number, text });
			delta--;
		} else {
			rows.push({ kind: "ctx", oldNo: number, newNo: number + delta, text });
		}
	}
	return sections.flatMap(trimContext);
}

export function takeRows(hunks: Hunk[], limit: number): { hunks: Hunk[]; hidden: number } {
	const taken: Hunk[] = [];
	let left = limit;
	let hidden = 0;
	for (const hunk of hunks) {
		if (left > 0) taken.push(hunk.slice(0, left));
		hidden += Math.max(0, hunk.length - left);
		left = Math.max(0, left - hunk.length);
	}
	return { hunks: taken.filter((hunk) => hunk.length > 0), hidden };
}

export function hunksFromContent(content: string): Hunk[] {
	const lines = content.replace(/\r\n?/g, "\n").replace(/\n$/, "").split("\n");
	return lines.length === 1 && lines[0] === "" ? [] : [lines.map((text, index) => ({ kind: "add" as const, newNo: index + 1, text: cleanText(text) }))];
}

function trimContext(rows: DiffRow[]): Hunk[] {
	const keep = rows.map(() => false);
	rows.forEach((row, index) => {
		if (row.kind === "ctx") return;
		for (let near = Math.max(0, index - CONTEXT_LINES); near <= Math.min(rows.length - 1, index + CONTEXT_LINES); near++) keep[near] = true;
	});
	const hunks: Hunk[] = [];
	rows.forEach((row, index) => {
		if (!keep[index]) return;
		if (index === 0 || !keep[index - 1]) hunks.push([]);
		hunks[hunks.length - 1]!.push(row);
	});
	return hunks;
}

function toBlocks(hunk: Hunk): Block[] {
	const blocks: Block[] = [];
	let change: { dels: DiffRow[]; adds: DiffRow[] } | undefined;
	for (const row of hunk) {
		if (row.kind === "ctx") {
			change = undefined;
			blocks.push({ ctx: row });
			continue;
		}
		if (!change) {
			change = { dels: [], adds: [] };
			blocks.push(change);
		}
		(row.kind === "del" ? change.dels : change.adds).push(row);
	}
	return blocks;
}

function markWords(dels: DiffRow[], adds: DiffRow[]): void {
	for (let index = 0; index < Math.min(dels.length, adds.length); index++) {
		const del = dels[index]!;
		const add = adds[index]!;
		const delMarks = new Array<boolean>(del.text.length).fill(false);
		const addMarks = new Array<boolean>(add.text.length).fill(false);
		let delAt = 0;
		let addAt = 0;
		for (const part of Diff.diffWordsWithSpace(del.text, add.text)) {
			if (part.removed) {
				delMarks.fill(true, delAt, delAt + part.value.length);
				delAt += part.value.length;
			} else if (part.added) {
				addMarks.fill(true, addAt, addAt + part.value.length);
				addAt += part.value.length;
			} else {
				delAt += part.value.length;
				addAt += part.value.length;
			}
		}
		const settle = (row: DiffRow, marks: boolean[]) => {
			const lead = row.text.length - row.text.trimStart().length;
			marks.fill(false, 0, lead);
			const body = row.text.length - lead;
			const marked = marks.filter(Boolean).length;
			if (body > 0 && marked / body <= MARK_LIMIT) row.marks = marks;
		};
		settle(del, delMarks);
		settle(add, addMarks);
	}
}

export function plainHunks(hunks: Hunk[]): string {
	const width = numberWidth(hunks);
	return hunks
		.map((hunk) => {
			const first = hunk[0];
			const rows = hunk.map((row) => `${String((row.kind === "del" ? row.oldNo : row.newNo) ?? "").padStart(width)} ${KIND_MARKER[row.kind]} ${row.text}`);
			return [`@@ line ${first?.newNo ?? first?.oldNo ?? 1} @@`, ...rows].join("\n");
		})
		.join("\n");
}

export function resolveStyle(style: DiffStyle, width: number): "unified" | "split" {
	if (style === "unified") return "unified";
	const pane = Math.floor((width - DIVIDER_WIDTH) / 2);
	return pane >= (style === "split" ? FORCED_SPLIT_MIN_PANE : SPLIT_MIN_PANE) ? "split" : "unified";
}

export function splitFits(width: number): boolean {
	return resolveStyle("auto", width) === "split";
}

export function twoSided(hunks: Hunk[]): boolean {
	return hunks.some((hunk) => hunk.some((row) => row.kind === "del") && hunk.some((row) => row.kind === "add"));
}

export interface DiffTheme {
	fg(color: string, text: string): string;
	bold?(text: string): string;
	inverse?(text: string): string;
	getFgAnsi?(color: string): string;
	getColorMode?(): string;
}

export interface DiffOptions {
	path?: string;
	wholeFile?: boolean;
	totalRows?: number;
}

interface Palette {
	rich: boolean;
	rowBg: { add: string; del: string };
	wordBg: { add: string; del: string };
}

const DARK = { add: [20, 54, 34], del: [66, 28, 32], addWord: [32, 104, 58], delWord: [128, 42, 52] };
const LIGHT = { add: [218, 244, 224], del: [252, 224, 227], addWord: [160, 224, 176], delWord: [246, 170, 178] };

function luminance(theme: DiffTheme): number | undefined {
	const match = theme.getFgAnsi?.("text")?.match(/^\x1b\[38;2;(\d+);(\d+);(\d+)m/);
	if (!match) return undefined;
	const channel = (value: string) => {
		const c = Number(value) / 255;
		return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
	};
	return 0.2126 * channel(match[1]!) + 0.7152 * channel(match[2]!) + 0.0722 * channel(match[3]!);
}

function paletteFor(theme: DiffTheme): Palette {
	if (theme.getColorMode?.() !== "truecolor") return { rich: false, rowBg: { add: "", del: "" }, wordBg: { add: "", del: "" } };
	const light = (luminance(theme) ?? 1) < 0.5;
	const tone = light ? LIGHT : DARK;
	const bg = (rgb: number[]) => `\x1b[48;2;${rgb.join(";")}m`;
	return { rich: true, rowBg: { add: bg(tone.add), del: bg(tone.del) }, wordBg: { add: bg(tone.addWord), del: bg(tone.delWord) } };
}

const KIND_COLOR = { add: "toolDiffAdded", del: "toolDiffRemoved", ctx: "toolDiffContext" } as const;
const KIND_MARKER = { add: "+", del: "-", ctx: " " } as const;
const RULE = "╌";

function numberWidth(hunks: Hunk[]): number {
	let max = 1;
	for (const row of hunks.flat()) max = Math.max(max, String(row.oldNo ?? 0).length, String(row.newNo ?? 0).length);
	return max;
}

function highlightHunk(hunk: Hunk, lang: string): void {
	const sides = [hunk.filter((row) => row.kind !== "add"), hunk.filter((row) => row.kind !== "del")];
	for (const rows of sides) {
		let lines: string[];
		try {
			lines = highlightCode(rows.map((row) => row.text).join("\n"), lang);
		} catch {
			continue;
		}
		if (lines.length !== rows.length) continue;
		rows.forEach((row, index) => {
			if (!row.ansi && stripTerminalSequences(lines[index]!) === row.text) row.ansi = lines[index];
		});
	}
}

function paint(cells: Cell[], row: DiffRow, palette: Palette, theme: DiffTheme): string {
	const rowBg = palette.rich && row.kind !== "ctx" ? palette.rowBg[row.kind] : "";
	let out = "";
	let index = 0;
	while (index < cells.length) {
		const first = cells[index]!;
		let end = index;
		let text = "";
		while (end < cells.length && cells[end]!.fg === first.fg && cells[end]!.attrs === first.attrs && cells[end]!.mark === first.mark) text += cells[end++]!.text;
		index = end;
		if (!palette.rich) {
			const colored = theme.fg(KIND_COLOR[row.kind], text);
			out += first.mark ? (theme.inverse ? theme.inverse(colored) : (theme.bold?.(colored) ?? colored)) : colored;
			continue;
		}
		let piece = text;
		if (first.fg || first.attrs) piece = `${first.attrs}${first.fg}${text}${first.fg ? "\x1b[39m" : ""}${first.attrs ? "\x1b[22m\x1b[23m\x1b[24m" : ""}`;
		else if (row.kind === "ctx") piece = theme.fg("toolDiffContext", text);
		if (first.mark && row.kind !== "ctx") piece = `${palette.wordBg[row.kind]}${piece}${rowBg || "\x1b[49m"}`;
		out += piece;
	}
	return out;
}

function rowLines(row: DiffRow, number: number | undefined, numWidth: number, width: number, palette: Palette, theme: DiffTheme, pad: boolean): string[] {
	const gutterWidth = numWidth + 3;
	const textWidth = Math.max(1, width - gutterWidth);
	row.cells ??= palette.rich && row.ansi ? ansiCells(row.ansi, row.marks) : toCells(row.text, row.marks);
	const rowBg = palette.rich && row.kind !== "ctx" ? palette.rowBg[row.kind] : "";
	return chunkCells(row.cells, textWidth).map((chunk, index) => {
		const first = index === 0;
		const gutter = theme.fg("dim", first ? String(number ?? "").padStart(numWidth) : " ".repeat(numWidth));
		const marker = first && row.kind !== "ctx" ? theme.fg(KIND_COLOR[row.kind], KIND_MARKER[row.kind]) : " ";
		const fill = rowBg || pad ? " ".repeat(Math.max(0, textWidth - cellsWidth(chunk))) : "";
		const line = `${gutter} ${marker} ${paint(chunk, row, palette, theme)}${fill}`;
		return rowBg ? `${rowBg}${line}\x1b[0m` : line;
	});
}

function rule(label: string, width: number, theme: DiffTheme): string {
	const lead = `${RULE}${RULE} ${label} `;
	return theme.fg("dim", lead + RULE.repeat(Math.max(2, width - visibleLength(lead))));
}

function visibleLength(text: string): number {
	return [...text].length;
}

export function renderHunks(hunks: Hunk[], width: number, theme: DiffTheme, style: DiffStyle, options: DiffOptions = {}): string[] {
	const palette = paletteFor(theme);
	const lang = palette.rich && options.path ? getLanguageFromPath(options.path) : undefined;
	const numWidth = numberWidth(hunks);
	const layout = resolveStyle(style, width);
	const pane = Math.floor((width - DIVIDER_WIDTH) / 2);
	const divider = ` ${theme.fg("dim", "│")} `;
	const total = options.totalRows ?? hunks.reduce((sum, hunk) => sum + hunk.length, 0);
	const out: string[] = [];
	hunks.forEach((hunk, hunkIndex) => {
		const first = hunk[0];
		if (!first) return;
		if (lang && supportsLanguage(lang)) highlightHunk(hunk, lang);
		const label = options.wholeFile && hunkIndex === 0 ? `${total} ${total === 1 ? "line" : "lines"} written` : `line ${first.newNo ?? first.oldNo}`;
		out.push(rule(label, width, theme));
		const blocks = toBlocks(hunk);
		for (const block of blocks) if (!("ctx" in block)) markWords(block.dels, block.adds);
		const split = layout === "split" && hunk.some((row) => row.kind === "del") && hunk.some((row) => row.kind === "add");
		for (const block of blocks) {
			if (!split) {
				const rows = "ctx" in block ? [block.ctx] : [...block.dels, ...block.adds];
				for (const row of rows) out.push(...rowLines(row, row.kind === "del" ? row.oldNo : row.newNo, numWidth, width, palette, theme, false));
				continue;
			}
			const pairs: Array<[DiffRow | undefined, DiffRow | undefined]> = [];
			if ("ctx" in block) pairs.push([block.ctx, block.ctx]);
			else for (let index = 0; index < Math.max(block.dels.length, block.adds.length); index++) pairs.push([block.dels[index], block.adds[index]]);
			for (const [del, add] of pairs) {
				const left = del ? rowLines(del, del.oldNo, numWidth, pane, palette, theme, true) : [];
				const right = add ? rowLines(add, add.newNo, numWidth, pane, palette, theme, false) : [];
				for (let line = 0; line < Math.max(left.length, right.length); line++) {
					out.push(`${left[line] ?? " ".repeat(pane)}${divider}${right[line] ?? ""}`);
				}
			}
		}
	});
	return out;
}

export class DiffView implements Component {
	private cache: { key: string; lines: string[] } | undefined;
	private readonly hunks: Hunk[];
	private readonly theme: DiffTheme;
	private readonly options: DiffOptions & { preview: boolean; style: () => DiffStyle };

	constructor(hunks: Hunk[], theme: DiffTheme, options: DiffOptions & { preview: boolean; style: () => DiffStyle }) {
		this.hunks = hunks;
		this.theme = theme;
		this.options = options;
	}

	render(width: number): string[] {
		const style = this.options.style();
		const key = `${width}:${style}:${this.options.preview}`;
		if (this.cache?.key === key) return this.cache.lines;
		let lines: string[];
		if (this.options.preview) {
			const shown = takeRows(this.hunks, PREVIEW_ROWS);
			const totalRows = this.hunks.reduce((sum, hunk) => sum + hunk.length, 0);
			const rendered = renderHunks(shown.hunks, Math.max(20, width), this.theme, style, { ...this.options, totalRows });
			const hidden = Math.max(0, rendered.length - PREVIEW_ROWS) + shown.hidden;
			lines = rendered.slice(0, PREVIEW_ROWS);
			if (hidden > 0) lines.push(this.theme.fg("dim", `… +${hidden} lines · ctrl+o`));
		} else {
			lines = renderHunks(this.hunks, Math.max(20, width), this.theme, style, this.options);
		}
		this.cache = { key, lines };
		return lines;
	}

	invalidate(): void {
		this.cache = undefined;
	}
}
