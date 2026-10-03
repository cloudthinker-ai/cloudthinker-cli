import { Editor } from "@earendil-works/pi-tui";

import { resolveConnectionMention } from "@cloudthinker/cloud/src/connection-mentions.ts";

import { UserMessageComponent } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/components/user-message.js";
import { theme } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/theme/theme.js";

const MENTION = /(^|[\s(])(#connection\/[A-Za-z0-9][\w-]*(?:\/[A-Za-z0-9][\w-]*)?|@[^\s"'=]+)/g;
const ESCAPE = /\x1b\[[0-9;]*m|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b_[^\x07\x1b]*(?:\x07|\x1b\\)/y;
const FULL_RESET = /^\x1b\[0?m$/;

export type MentionKind = "connection" | "missing" | "agent" | "file";

export function mentionKind(token: string): MentionKind {
	if (token.startsWith("#connection/")) {
		const [prefix, alias] = token.slice("#connection/".length).split("/");
		return resolveConnectionMention(prefix!, alias).status === "unknown" ? "missing" : "connection";
	}
	return /[/.]/.test(token.slice(1)) ? "file" : "agent";
}

function styleOf(kind: MentionKind): { open: string; close: string } {
	const [open = "", close = ""] = theme.fg(kind === "missing" ? "error" : "accent", "\u0000").split("\u0000");
	return { open, close };
}

export function highlightMentions(line: string): string {
	const visible: number[] = [];
	let plain = "";
	const escapes: { at: number; text: string }[] = [];
	for (let index = 0; index < line.length; ) {
		ESCAPE.lastIndex = index;
		const escape = ESCAPE.exec(line);
		if (escape) {
			escapes.push({ at: index, text: escape[0] });
			index += escape[0].length;
			continue;
		}
		visible.push(index);
		plain += line[index];
		index += 1;
	}
	visible.push(line.length);
	const inserts: { at: number; text: string; order: number }[] = [];
	for (const match of plain.matchAll(MENTION)) {
		const token = match[2]!;
		const start = match.index! + match[1]!.length;
		const end = start + token.length;
		const style = styleOf(mentionKind(token));
		const rawStart = visible[start]!;
		const rawEnd = visible[end]!;
		inserts.push({ at: rawStart, text: style.open, order: 1 });
		for (const escape of escapes) {
			if (escape.at > rawStart && escape.at < rawEnd && FULL_RESET.test(escape.text)) inserts.push({ at: escape.at + escape.text.length, text: style.open, order: 1 });
		}
		inserts.push({ at: rawEnd, text: style.close, order: 0 });
	}
	if (inserts.length === 0) return line;
	inserts.sort((left, right) => right.at - left.at || right.order - left.order);
	let out = line;
	for (const insert of inserts) out = out.slice(0, insert.at) + insert.text + out.slice(insert.at);
	return out;
}

export function applyMentionHighlight(): void {
	const editor = Editor.prototype as unknown as { render(this: { renderedVisibleLineCount: number }, width: number): string[] };
	const message = UserMessageComponent.prototype as unknown as { render(width: number): string[] };
	const renderEditor = editor.render;
	const renderMessage = message.render;
	if (typeof renderEditor !== "function" || typeof renderMessage !== "function") {
		throw new Error("pi's editor or user message renderer changed, so mentions cannot be highlighted");
	}
	editor.render = function (width) {
		const lines = renderEditor.call(this, width);
		for (let index = 1; index <= this.renderedVisibleLineCount && index < lines.length; index++) lines[index] = highlightMentions(lines[index]!);
		return lines;
	};
	message.render = function (width) {
		return renderMessage.call(this, width).map(highlightMentions);
	};
}
