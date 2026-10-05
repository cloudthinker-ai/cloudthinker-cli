import type { Component } from "@earendil-works/pi-tui";
import { visibleWidth } from "@earendil-works/pi-tui";

import { sanitizeTerminalText } from "@cloudthinker/cloud/src/awareness.ts";

import { chunkCells, toCells } from "./wrap.ts";

export const HEREDOC_PREVIEW_LINES = 4;

export interface Segment {
	text: string;
	body: string[];
	end: string[];
}

interface PendingHeredoc {
	delimiter: string;
	strip: boolean;
	segment: Segment;
}

const OPERATOR_ONLY = /^(&&|\|\||\|&|;|\|)$/;
const DELIMITER_END = /[\s;&|<>()]/;

function readHeredocTag(source: string, from: number): { end: number; delimiter: string; strip: boolean } | undefined {
	let at = from + 2;
	const strip = source[at] === "-";
	if (strip) at++;
	while (source[at] === " " || source[at] === "\t") at++;
	const quote = source[at] === "'" || source[at] === '"' ? source[at]! : undefined;
	let delimiter = "";
	if (quote) {
		const close = source.indexOf(quote, at + 1);
		if (close === -1) return undefined;
		delimiter = source.slice(at + 1, close);
		at = close + 1;
	} else {
		while (at < source.length && !DELIMITER_END.test(source[at]!)) {
			if (source[at] === "\\") at++;
			else delimiter += source[at];
			at++;
		}
	}
	return delimiter ? { end: at, delimiter, strip } : undefined;
}

export function splitCommand(command: string): Segment[] {
	const source = command.replace(/\r\n?/g, "\n").trim();
	const segments: Segment[] = [];
	const pending: PendingHeredoc[] = [];
	let segment: Segment = { text: "", body: [], end: [] };
	let quote: string | undefined;
	let depth = 0;

	const flush = (operator: string) => {
		segment.text = segment.text.replace(/[ \t]+$/, "");
		if (segment.text.trim() || segment.body.length > 0 || segment.end.length > 0) segments.push(segment);
		segment = { text: operator ? `${operator} ` : "", body: [], end: [] };
	};
	const atStart = () => segment.text === "" || OPERATOR_ONLY.test(segment.text.trim());

	let i = 0;
	while (i < source.length) {
		const char = source[i]!;
		if (quote) {
			segment.text += char;
			if (char === "\\" && quote !== "'" && i + 1 < source.length) {
				segment.text += source[i + 1];
				i++;
			} else if (char === quote) quote = undefined;
			i++;
			continue;
		}
		if (char === "\\" && i + 1 < source.length) {
			segment.text += char + source[i + 1];
			i += 2;
			continue;
		}
		if (char === "'" || char === '"' || char === "`") {
			quote = char;
			segment.text += char;
			i++;
			continue;
		}
		if (char === "#" && (i === 0 || /\s/.test(source[i - 1]!))) {
			const stop = source.indexOf("\n", i);
			const end = stop === -1 ? source.length : stop;
			segment.text += source.slice(i, end);
			i = end;
			continue;
		}
		if (char === "(") depth++;
		else if (char === ")") depth = Math.max(0, depth - 1);
		if (char === "<" && source[i + 1] === "<" && depth === 0) {
			if (source[i + 2] === "<") {
				segment.text += "<<<";
				i += 3;
				continue;
			}
			const tag = readHeredocTag(source, i);
			if (tag) {
				pending.push({ delimiter: tag.delimiter, strip: tag.strip, segment });
				segment.text += source.slice(i, tag.end);
				i = tag.end;
				continue;
			}
		}
		if (char === "\n") {
			if (depth > 0) {
				segment.text += char;
				i++;
				continue;
			}
			let next = i + 1;
			for (const heredoc of pending.splice(0)) {
				for (;;) {
					if (next > source.length) break;
					const stop = source.indexOf("\n", next);
					const lineEnd = stop === -1 ? source.length : stop;
					const line = source.slice(next, lineEnd);
					next = lineEnd + 1;
					if ((heredoc.strip ? line.replace(/^\t+/, "") : line) === heredoc.delimiter) {
						heredoc.segment.end.push(line);
						break;
					}
					heredoc.segment.body.push(line);
					if (stop === -1) break;
				}
			}
			if (atStart()) segment.text += " ";
			else flush("");
			i = Math.min(next, source.length);
			continue;
		}
		if (depth === 0) {
			const pair = source.slice(i, i + 2);
			const operator = pair === "&&" || pair === "||" || pair === "|&" || pair === ";;" ? pair : char === ";" || char === "|" ? char : undefined;
			if (operator === ";;") {
				segment.text += pair;
				i += 2;
				continue;
			}
			if (operator) {
				flush(operator);
				i += operator.length;
				continue;
			}
		}
		if ((char === " " || char === "\t") && atStart()) {
			i++;
			continue;
		}
		segment.text += char;
		i++;
	}
	flush("");
	return segments;
}

interface CommandTheme {
	fg(color: string, text: string): string;
	bold(text: string): string;
}

const clean = (line: string) => sanitizeTerminalText(line.replace(/\t/g, "  ").replace(/\s+$/, ""));

export class CommandBlock implements Component {
	private readonly segments: Segment[];
	private readonly prompt: string;
	private readonly theme: CommandTheme;
	private readonly options: { fold: boolean; indent: number };

	constructor(command: string, prompt: string, theme: CommandTheme, options: { fold: boolean; indent: number }) {
		this.segments = splitCommand(command);
		this.prompt = prompt;
		this.theme = theme;
		this.options = options;
	}

	render(width: number): string[] {
		const theme = this.theme;
		const indent = " ".repeat(this.options.indent);
		const lead = visibleWidth(this.prompt) + 1;
		const room = Math.max(10, width - this.options.indent - lead);
		const gutter = `${" ".repeat(lead - 2)}${theme.fg("muted", "│")} `;
		const title = (text: string) => theme.fg("toolTitle", theme.bold(text));
		const dim = (text: string) => theme.fg("dim", text);
		const out: string[] = [];
		const push = (text: string, style: (text: string) => string) => {
			const chunks = chunkCells(toCells(clean(text)), room, room - 2);
			chunks.forEach((chunk, index) => {
				const body = chunk.map((cell) => cell.text).join("");
				const prefix = out.length === 0 ? `${title(this.prompt)} ` : index === 0 ? gutter : `${gutter}  `;
				out.push(`${out.length === 0 ? "" : indent}${prefix}${style(body)}`);
			});
		};
		for (const segment of this.segments) {
			for (const line of segment.text.split("\n")) push(line, title);
			const fold = this.options.fold && segment.body.length > HEREDOC_PREVIEW_LINES;
			for (const line of fold ? segment.body.slice(0, HEREDOC_PREVIEW_LINES) : segment.body) push(line, dim);
			if (fold) push(`… +${segment.body.length - HEREDOC_PREVIEW_LINES} lines`, dim);
			for (const line of segment.end) push(line, dim);
		}
		return out;
	}

	invalidate(): void {}
}
