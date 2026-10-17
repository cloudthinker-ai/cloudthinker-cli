import { Markdown, TuiAltScreen, sliceByColumn, stripTerminalSequences, type Token } from "@earendil-works/pi-tui";

import { labelNextCopy, plural } from "./terminal-copy.ts";

interface Origin {
	token: number;
	line: number;
	fragment: number;
}

interface Rendered {
	paddingX?: number;
	cachedText?: string;
	cachedWidth?: number;
	cachedLines?: readonly string[];
	cachedTokens?: WeakRef<{ tokens: readonly Token[] }>;
	lineOrigins?: Int32Array;
	render(width: number): string[];
}

interface Placed {
	md: Rendered;
	index: number;
}

export interface SelectedRow {
	line: string;
	start: number;
	end: number;
}

interface Picked {
	text: string;
	plain: string;
	full: boolean;
	md?: Rendered;
	index?: number;
	origin?: Origin;
}

interface TableToken {
	header: readonly { text: string }[];
	rows: readonly (readonly { text: string }[])[];
	align: readonly (string | null)[];
}

interface Piece {
	kind: "blank" | "text" | "markdown" | "table" | "code";
	text: string;
	count?: number;
	lang?: string;
}

export interface MarkdownCopy {
	text: string;
	label?: string;
}

const OSC133 = /\x1b\]133;[^\x07\x1b]*(?:\x07|\x1b\\)/g;
const OSC8 = /\x1b\]8;[^;\x07\x1b]*;([^\x07\x1b]*)(?:\x07|\x1b\\)(.*?)\x1b\]8;[^;\x07\x1b]*;(?:\x07|\x1b\\)/g;

const live = new Set<WeakRef<Rendered>>();
const tracked = new WeakSet<object>();
let sweepAt = 256;

function key(line: string): string {
	return stripTerminalSequences(line.replace(OSC133, "")).trim();
}

function withLinks(slice: string): string {
	return stripTerminalSequences(slice.replace(OSC8, (_, url: string, label: string) => {
		const text = stripTerminalSequences(label);
		return url && text !== url ? `[${text}](${url})` : text;
	})).trimEnd();
}

function track(md: Rendered): void {
	if (tracked.has(md)) return;
	tracked.add(md);
	live.add(new WeakRef(md));
	if (live.size < sweepAt) return;
	for (const ref of live) if (!ref.deref()) live.delete(ref);
	sweepAt = Math.max(256, live.size * 2);
}

function originAt(md: Rendered, index: number): Origin | undefined {
	const origins = md.lineOrigins!;
	const token = origins[index * 3]!;
	return token < 0 ? undefined : { token, line: origins[index * 3 + 1]!, fragment: origins[index * 3 + 2]! };
}

function placements(): Map<string, Placed[]> {
	const found = new Map<string, Placed[]>();
	for (const ref of live) {
		const md = ref.deref();
		if (!md) {
			live.delete(ref);
			continue;
		}
		const lines = md.cachedLines;
		if (!lines || md.lineOrigins?.length !== lines.length * 3) continue;
		lines.forEach((line, index) => {
			const text = key(line);
			if (!text) return;
			const list = found.get(text);
			if (list) list.push({ md, index });
			else found.set(text, [{ md, index }]);
		});
	}
	return found;
}

function place(rows: readonly SelectedRow[], found: Map<string, Placed[]>): (Placed | undefined)[] {
	const keys = rows.map((row) => key(row.line));
	const run = (at: Placed, from: number) => {
		const lines = at.md.cachedLines!;
		let length = 0;
		while (from + length < keys.length && at.index + length < lines.length && key(lines[at.index + length]!) === keys[from + length]) length++;
		return length;
	};
	const placed: (Placed | undefined)[] = [];
	let current: { at: Placed; row: number } | undefined;
	keys.forEach((text, i) => {
		if (current) {
			const index = current.at.index + i - current.row;
			const line = current.at.md.cachedLines![index];
			if (line !== undefined && key(line) === text) {
				placed.push({ md: current.at.md, index });
				return;
			}
		}
		const options = text ? found.get(text) : undefined;
		if (!options) {
			placed.push(undefined);
			current = undefined;
			return;
		}
		let best = options[0]!;
		let bestRun = 0;
		for (const option of options) {
			const length = run(option, i);
			if (length >= bestRun) {
				best = option;
				bestRun = length;
			}
		}
		placed.push(best);
		current = { at: best, row: i };
	});
	return placed;
}

function tokensOf(md: Rendered): readonly Token[] | undefined {
	const cached = md.cachedTokens?.deref()?.tokens;
	if (cached || md.cachedWidth === undefined) return cached;
	md.cachedText = undefined;
	md.render(md.cachedWidth);
	return md.cachedTokens?.deref()?.tokens;
}

function tokenLines(md: Rendered, token: number): number[] {
	const indexes: number[] = [];
	for (let index = 0; index < md.cachedLines!.length; index++) {
		if (md.lineOrigins![index * 3] === token) indexes.push(index);
	}
	return indexes;
}

function isRule(line: string): boolean {
	const text = key(line);
	return text.length > 0 && /^[─\s]+$/.test(text);
}

function cell(text: string): string {
	return text.replace(/\|/g, "\\|").replace(/\s*\n\s*/g, " ").trim();
}

export function markdownTable(table: TableToken, rows: readonly number[]): string {
	const marker = (align: string | null) => (align === "left" ? ":---" : align === "right" ? "---:" : align === "center" ? ":---:" : "---");
	const line = (cells: readonly { text: string }[]) => `| ${cells.map((value) => cell(value.text)).join(" | ")} |`;
	return [line(table.header), `| ${table.align.map(marker).join(" | ")} |`, ...rows.map((row) => line(table.rows[row]!))].join("\n");
}

function margin(md: Rendered | undefined, row: SelectedRow, text: string): string {
	const left = (md?.paddingX ?? 0) - row.start;
	return left > 0 ? text.replace(new RegExp(`^ {0,${left}}`), "") : text;
}

function joiner(before: string, after: string, source: string, fallback: string): string {
	const tail = before.trimEnd().split(/\s/).at(-1);
	const head = after.trimStart().split(/\s/)[0];
	if (!tail || !head) return fallback;
	if (source.includes(`${tail} ${head}`)) return " ";
	if (source.includes(`${tail}${head}`)) return "";
	return fallback;
}

function joinFragments(group: readonly Picked[], source: string, fallback: string): string[] {
	const lines: string[] = [];
	let previous: Origin | undefined;
	for (const row of group) {
		if (!row.plain.trim()) continue;
		if (previous && row.origin && row.origin.fragment > 0 && row.origin.line === previous.line) {
			const last = lines[lines.length - 1]!;
			lines[lines.length - 1] = last + joiner(last, row.text, source, fallback) + row.text.trimStart();
		} else lines.push(row.text);
		previous = row.origin;
	}
	return lines;
}

function tablePiece(md: Rendered, tokenIndex: number, table: TableToken, group: readonly Picked[]): Piece | undefined {
	const lines = md.cachedLines!;
	const rules = tokenLines(md, tokenIndex).filter((index) => isRule(lines[index]!));
	if (rules.length === 0) return undefined;
	const covered = new Set<number>();
	for (const row of group) {
		if (!row.plain.trim() || row.index === undefined || isRule(lines[row.index]!)) continue;
		const before = rules.filter((rule) => rule < row.index!).length;
		if (before > 0) covered.add(before - 1);
	}
	const rows = [...covered].filter((row) => row < table.rows.length).sort((a, b) => a - b);
	if (rows.length === 0) return undefined;
	return { kind: "table", text: markdownTable(table, rows), count: rows.length };
}

function codePiece(md: Rendered, tokens: readonly Token[], tokenIndex: number, group: readonly Picked[]): Piece | undefined {
	const token = tokens[tokenIndex] as unknown as { text: string; lang?: string };
	const code = token.text.split("\n");
	const indexes = tokenLines(md, tokenIndex);
	const lineCount = Math.max(...indexes.map((index) => originAt(md, index)!.line)) + 1;
	const next = tokens[tokenIndex + 1];
	const offset = lineCount - (next && next.type !== "space" ? 1 : 0) - code.length;
	if (offset < 0) return undefined;
	const byLine = new Map<number, Picked[]>();
	for (const row of group) {
		const line = row.origin!.line - offset;
		if (line < 0 || line >= code.length) continue;
		byLine.set(line, [...(byLine.get(line) ?? []), row]);
	}
	if (byLine.size === 0) return { kind: "blank", text: "" };
	const lines = [...byLine.keys()].sort((a, b) => a - b).map((line) => {
		const rows = byLine.get(line)!;
		const fragments = indexes.filter((index) => originAt(md, index)!.line === line + offset).length;
		if (rows.length === fragments && rows.every((row) => row.full)) return code[line]!;
		return joinFragments(rows.map((row) => ({ ...row, text: row.plain })), code[line]!, "").join("");
	});
	return { kind: "code", text: lines.join("\n"), lang: token.lang, count: lines.length };
}

function textPiece(group: readonly Picked[], source: string, skip?: (row: Picked) => boolean): Piece {
	return { kind: "text", text: joinFragments(skip ? group.filter((row) => !skip(row)) : group, source, " ").join("\n") };
}

function groupPiece(group: readonly Picked[]): Piece {
	const md = group[0]!.md!;
	const tokenIndex = group[0]!.origin!.token;
	const tokens = tokensOf(md);
	const token = tokens?.[tokenIndex];
	const raw = (token as { raw?: string } | undefined)?.raw ?? "";
	if (!tokens || !token) return textPiece(group, raw);
	if (token.type === "space") return { kind: "blank", text: "" };
	if (token.type === "table") {
		const lines = md.cachedLines!;
		return tablePiece(md, tokenIndex, token as unknown as TableToken, group) ?? textPiece(group, raw, (row) => row.index !== undefined && isRule(lines[row.index]!));
	}
	if (token.type === "code") return codePiece(md, tokens, tokenIndex, group) ?? textPiece(group, raw);
	const shown = tokenLines(md, tokenIndex).filter((index) => key(md.cachedLines![index]!)).length;
	const selected = group.filter((row) => row.plain.trim());
	if (raw && selected.length === shown && selected.every((row) => row.full)) return { kind: "markdown", text: raw.replace(/\s+$/, "") };
	return textPiece(group, raw);
}

export function markdownSelection(rows: readonly SelectedRow[]): MarkdownCopy | undefined {
	if (rows.length < 2) return undefined;
	const placed = place(rows, placements());
	if (placed.every((at) => !at)) return undefined;
	const picked: Picked[] = rows.map((row, i) => {
		const at = placed[i];
		const slice = sliceByColumn(row.line, row.start, Math.max(0, row.end - row.start), true);
		const plain = margin(at?.md, row, stripTerminalSequences(slice).trimEnd());
		return {
			text: margin(at?.md, row, withLinks(slice)),
			plain,
			full: plain.trim() === key(row.line),
			md: at?.md,
			index: at?.index,
			origin: at ? originAt(at.md, at.index) : undefined,
		};
	});
	const pieces: Piece[] = [];
	for (let i = 0; i < picked.length; ) {
		const row = picked[i]!;
		if (!row.md || !row.origin) {
			pieces.push(row.plain.trim() ? { kind: "text", text: row.text } : { kind: "blank", text: "" });
			i++;
			continue;
		}
		let end = i;
		while (end + 1 < picked.length && picked[end + 1]!.md === row.md && picked[end + 1]!.origin?.token === row.origin.token) end++;
		const group = picked.slice(i, end + 1);
		const lead = group.findIndex((value) => value.plain.trim());
		if (lead > 0) pieces.push({ kind: "blank", text: "" });
		pieces.push(groupPiece(group));
		if (lead >= 0 && !group.at(-1)!.plain.trim()) pieces.push({ kind: "blank", text: "" });
		i = end + 1;
	}
	const content = pieces.filter((piece) => piece.kind !== "blank" && piece.text);
	const codeOnly = content.length === 1 && content[0]!.kind === "code";
	const out: string[] = [];
	for (const piece of pieces) {
		if (piece.kind === "blank") {
			if (out.length > 0 && out.at(-1) !== "") out.push("");
			continue;
		}
		if (!piece.text) continue;
		out.push(piece.kind === "code" && !codeOnly ? `\`\`\`${piece.lang ?? ""}\n${piece.text}\n\`\`\`` : piece.text);
	}
	while (out.at(-1) === "") out.pop();
	const text = out.join("\n");
	const original = picked.map((row) => row.plain).join("\n");
	if (!text || text === original) return undefined;
	const only = content.length === 1 ? content[0]! : undefined;
	if (only?.kind === "table") return { text, label: `table (${plural(only.count ?? 0, "row")}) as Markdown` };
	if (codeOnly) return { text, label: `code (${plural(only!.count ?? 0, "line")})` };
	return { text, label: `${plural([...text].length, "char")} as Markdown` };
}

interface SelectionScreen {
	getActiveSelectionText(): string | undefined;
	getSelectionColumns(line: string, row: number, selection: unknown, minColumn?: number, maxColumn?: number): { start: number; end: number };
	copySelectionToClipboard(): Promise<boolean>;
	copyActiveSelectionToClipboard(): Promise<boolean>;
}

export function applyMarkdownCopy(): void {
	const markdown = Markdown.prototype as unknown as { render(this: Rendered, width: number): string[] };
	const { render } = markdown;
	markdown.render = function (width) {
		const lines = render.call(this, width);
		track(this);
		return lines;
	};
	const screen = TuiAltScreen.prototype as unknown as SelectionScreen;
	const { getActiveSelectionText, getSelectionColumns, copySelectionToClipboard, copyActiveSelectionToClipboard } = screen;
	if ([getActiveSelectionText, getSelectionColumns, copySelectionToClipboard, copyActiveSelectionToClipboard].some((method) => typeof method !== "function")) {
		throw new Error("pi-tui's fullscreen selection no longer copies through getActiveSelectionText and getSelectionColumns, so a copy cannot paste as Markdown");
	}
	const copying = new WeakSet<object>();
	const capture = new WeakMap<object, SelectedRow[]>();
	screen.getSelectionColumns = function (line, row, selection, minColumn, maxColumn) {
		const columns = getSelectionColumns.call(this, line, row, selection, minColumn, maxColumn);
		capture.get(this)?.push({ line, start: columns.start, end: columns.end });
		return columns;
	};
	screen.getActiveSelectionText = function () {
		if (!copying.has(this)) return getActiveSelectionText.call(this);
		copying.delete(this);
		const rows: SelectedRow[] = [];
		capture.set(this, rows);
		let text: string | undefined;
		try {
			text = getActiveSelectionText.call(this);
		} finally {
			capture.delete(this);
		}
		if (text === undefined) return text;
		const copied = markdownSelection(rows);
		if (!copied) return text;
		if (copied.label) labelNextCopy(this, copied.text, copied.label);
		return copied.text;
	};
	const copyingWith = (copy: () => Promise<boolean>) =>
		function (this: object) {
			copying.add(this);
			try {
				return copy.call(this);
			} finally {
				copying.delete(this);
			}
		};
	screen.copySelectionToClipboard = copyingWith(copySelectionToClipboard);
	screen.copyActiveSelectionToClipboard = copyingWith(copyActiveSelectionToClipboard);
}
