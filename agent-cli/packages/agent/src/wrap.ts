import { visibleWidth } from "@earendil-works/pi-tui";

export interface Cell {
	text: string;
	width: number;
	mark: boolean;
	fg: string;
	attrs: string;
}

const segmenter = new Intl.Segmenter();
const SGR = /\x1b\[([0-9;]*)m/g;
const ATTR_OPEN: Record<number, string> = { 1: "\x1b[1m", 2: "\x1b[2m", 3: "\x1b[3m", 4: "\x1b[4m" };

function pushCells(cells: Cell[], text: string, start: number, marks: boolean[] | undefined, fg: string, attrs: string): void {
	for (const { segment, index } of segmenter.segment(text)) {
		cells.push({ text: segment, width: visibleWidth(segment), mark: marks?.[start + index] ?? false, fg, attrs });
	}
}

export function toCells(text: string, marks?: boolean[]): Cell[] {
	const cells: Cell[] = [];
	pushCells(cells, text, 0, marks, "", "");
	return cells;
}

export function ansiCells(ansi: string, marks?: boolean[]): Cell[] {
	const cells: Cell[] = [];
	let fg = "";
	const attrs = new Set<number>();
	let plainAt = 0;
	let last = 0;
	const emit = (text: string) => {
		if (!text) return;
		pushCells(cells, text, plainAt, marks, fg, [...attrs].sort().map((code) => ATTR_OPEN[code]).join(""));
		plainAt += text.length;
	};
	for (const match of ansi.matchAll(SGR)) {
		emit(ansi.slice(last, match.index));
		last = match.index + match[0].length;
		const params = match[1]!.split(";").map((value) => Number(value || 0));
		for (let at = 0; at < params.length; at++) {
			const code = params[at]!;
			if (code === 0) {
				fg = "";
				attrs.clear();
			} else if (code === 39) fg = "";
			else if (code === 38) {
				const take = params[at + 1] === 2 ? 5 : params[at + 1] === 5 ? 3 : 1;
				fg = `\x1b[${params.slice(at, at + take).join(";")}m`;
				at += take - 1;
			} else if ((code >= 30 && code <= 37) || (code >= 90 && code <= 97)) fg = `\x1b[${code}m`;
			else if (code >= 1 && code <= 4) attrs.add(code);
			else if (code === 22) {
				attrs.delete(1);
				attrs.delete(2);
			} else if (code === 23) attrs.delete(3);
			else if (code === 24) attrs.delete(4);
		}
	}
	emit(ansi.slice(last));
	return cells;
}

export function cellsWidth(cells: Cell[]): number {
	return cells.reduce((sum, cell) => sum + cell.width, 0);
}

const isSpace = (cell: Cell) => cell.text === " " || cell.text === "\t";

export function chunkCells(cells: Cell[], first: number, rest: number = first): Cell[][] {
	const tokens: Cell[][] = [];
	for (const cell of cells) {
		const tail = tokens[tokens.length - 1];
		if (tail && isSpace(tail[0]!) === isSpace(cell)) tail.push(cell);
		else tokens.push([cell]);
	}
	const lines: Cell[][] = [[]];
	let used = 0;
	let limit = Math.max(1, first);
	let content = false;
	const next = () => {
		const line = lines[lines.length - 1]!;
		while (line.length > 0 && isSpace(line[line.length - 1]!)) line.pop();
		lines.push([]);
		used = 0;
		content = false;
		limit = Math.max(1, rest);
	};
	for (const token of tokens) {
		const width = cellsWidth(token);
		if (isSpace(token[0]!)) {
			if (used === 0 && lines.length > 1) continue;
			if (used + width > limit) {
				next();
				continue;
			}
			lines[lines.length - 1]!.push(...token);
			used += width;
			continue;
		}
		if (used + width > limit) {
			if (content) next();
			else {
				lines[lines.length - 1]!.length = 0;
				used = 0;
			}
		}
		if (width <= limit - used) {
			lines[lines.length - 1]!.push(...token);
			used += width;
			content = true;
			continue;
		}
		for (const cell of token) {
			if (used + cell.width > limit && used > 0) next();
			lines[lines.length - 1]!.push(cell);
			used += cell.width;
			content = true;
		}
	}
	const tail = lines[lines.length - 1]!;
	while (lines.length > 1 && tail.length > 0 && isSpace(tail[tail.length - 1]!)) tail.pop();
	return lines;
}
