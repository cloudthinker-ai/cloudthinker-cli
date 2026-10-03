import { InteractiveMode, type SessionEntry, type Theme } from "@earendil-works/pi-coding-agent";
import {
	type Component,
	decodeKittyPrintable,
	matchesKey,
	type TUI,
	type TuiMouseEvent,
	type TuiMouseEventResult,
	truncateToWidth,
	visibleWidth,
	wrapTextWithAnsi,
} from "@earendil-works/pi-tui";

import { copyToClipboard } from "@earendil-works/pi-coding-agent";
import { editInExternalEditor } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/external-editor.js";

export const TRANSCRIPT_TITLE = "Transcript";
export const EMPTY_TRANSCRIPT = "Nothing in this session yet.";
export const MAX_RESULT_LINES = 400;

export type BlockKind = "user" | "thinking" | "assistant" | "call" | "result" | "error" | "shell" | "note";

export interface TranscriptBlock {
	kind: BlockKind;
	title: string;
	body: string;
}

type Content = string | readonly { type: string; text?: string; thinking?: string; name?: string; arguments?: unknown }[];

function contentText(content: Content | undefined): string {
	if (content === undefined) return "";
	if (typeof content === "string") return content;
	return content
		.map((block) => (block.type === "text" ? (block.text ?? "") : block.type === "image" ? "[image]" : ""))
		.filter((text) => text.length > 0)
		.join("\n");
}

function capped(text: string): string {
	const lines = text.split("\n");
	if (lines.length <= MAX_RESULT_LINES) return text;
	const hidden = lines.length - MAX_RESULT_LINES;
	return [...lines.slice(0, MAX_RESULT_LINES), `… ${hidden} more lines (press e to read everything in your editor)`].join("\n");
}

function argumentsText(args: unknown): string {
	if (args === undefined || args === null) return "";
	if (typeof args !== "object") return String(args);
	const record = args as Record<string, unknown>;
	const keys = Object.keys(record);
	if (keys.length === 1 && typeof record[keys[0]!] === "string") return record[keys[0]!] as string;
	return JSON.stringify(record, null, 2);
}

export function transcriptBlocks(entries: readonly SessionEntry[], full = false): TranscriptBlock[] {
	const limit = full ? (text: string) => text : capped;
	const blocks: TranscriptBlock[] = [];
	for (const entry of entries) {
		if (entry.type === "compaction") {
			blocks.push({ kind: "note", title: "Earlier conversation (compacted)", body: entry.summary });
			continue;
		}
		if (entry.type === "branch_summary") {
			blocks.push({ kind: "note", title: "Branch summary", body: entry.summary });
			continue;
		}
		if (entry.type === "custom_message") {
			if (entry.display) blocks.push({ kind: "note", title: entry.customType, body: contentText(entry.content as Content) });
			continue;
		}
		if (entry.type !== "message") continue;
		const message = entry.message as unknown as { role: string } & Record<string, unknown>;
		if (message.role === "user") {
			blocks.push({ kind: "user", title: "You", body: contentText(message.content as Content) });
		} else if (message.role === "assistant") {
			const content = (message.content ?? []) as Exclude<Content, string>;
			const thinking = content.map((block) => (block.type === "thinking" ? (block.thinking ?? "").trim() : "")).filter(Boolean).join("\n\n");
			if (thinking) blocks.push({ kind: "thinking", title: "Thinking", body: thinking });
			const text = contentText(content.filter((block) => block.type === "text"));
			if (text.trim()) blocks.push({ kind: "assistant", title: "CloudThinker", body: text });
			for (const block of content) {
				if (block.type === "toolCall") {
					blocks.push({ kind: "call", title: block.name ?? "tool", body: limit(argumentsText(block.arguments)) });
				}
			}
			if (message.stopReason === "error" && typeof message.errorMessage === "string") {
				blocks.push({ kind: "error", title: "turn failed", body: message.errorMessage });
			}
		} else if (message.role === "toolResult") {
			blocks.push({
				kind: message.isError ? "error" : "result",
				title: `${String(message.toolName)} ${message.isError ? "failed" : "result"}`,
				body: limit([contentText(message.content as Content), (message.details as { diff?: unknown } | undefined)?.diff].filter((part) => typeof part === "string" && part).join("\n\n")),
			});
		} else if (message.role === "bashExecution") {
			const exit = message.cancelled ? "cancelled" : `exit ${String(message.exitCode ?? "?")}`;
			blocks.push({
				kind: "shell",
				title: `$ ${String(message.command)} (${exit})`,
				body: limit(String(message.output ?? "")),
			});
		} else if (message.role === "custom" && message.display) {
			blocks.push({ kind: "note", title: String(message.customType), body: contentText(message.content as Content) });
		}
	}
	return blocks;
}

export function transcriptMarkdown(entries: readonly SessionEntry[]): string {
	const sections = transcriptBlocks(entries, true).map((block) => {
		const fenced = block.kind === "call" || block.kind === "result" || block.kind === "error" || block.kind === "shell";
		const body = fenced ? `\`\`\`\n${block.body}\n\`\`\`` : block.body;
		return `## ${block.title}\n\n${body}`;
	});
	return `# ${TRANSCRIPT_TITLE}\n\n${sections.join("\n\n")}\n`;
}

const MARKS: Record<BlockKind, { mark: string; color: "accent" | "text" | "muted" | "error" | "warning" | "success" | "dim" }> = {
	user: { mark: "›", color: "accent" },
	thinking: { mark: "∴", color: "dim" },
	assistant: { mark: "●", color: "text" },
	call: { mark: "⏺", color: "muted" },
	result: { mark: "⎿", color: "dim" },
	error: { mark: "error:", color: "error" },
	shell: { mark: "$", color: "warning" },
	note: { mark: "◇", color: "muted" },
};

export function renderBlocks(blocks: readonly TranscriptBlock[], theme: Theme, width: number, starts: number[] = []): string[] {
	const inner = Math.max(10, width - 4);
	const lines: string[] = [];
	for (const block of blocks) {
		starts.push(lines.length);
		const { mark, color } = MARKS[block.kind];
		const title = block.kind === "user" || block.kind === "assistant" ? theme.bold(block.title) : block.title;
		lines.push(truncateToWidth(` ${theme.fg(color, mark)} ${theme.fg(color, title)}`, width));
		const bodyColor = block.kind === "result" || block.kind === "call" ? "muted" : block.kind === "thinking" ? "dim" : block.kind === "error" ? "error" : "text";
		for (const raw of block.body.replace(/\t/g, "  ").split("\n")) {
			for (const wrapped of wrapTextWithAnsi(raw, inner)) {
				lines.push(`   ${theme.fg(bodyColor, wrapped)}`);
			}
		}
		lines.push("");
	}
	return lines;
}

function plain(line: string): string {
	return line.replace(/\x1b\[[0-9;:]*[A-Za-z]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)/g, "");
}

export interface TranscriptViewOptions {
	entries: () => readonly SessionEntry[];
	theme: Theme;
	tui: Pick<TUI, "terminal" | "requestRender">;
	onClose: () => void;
	onOpenEditor: (markdown: string) => void;
	onCopy: (text: string) => Promise<void>;
}

export class TranscriptView implements Component {
	private readonly options: TranscriptViewOptions;
	private lines: string[] = [];
	private plainLines: string[] = [];
	private blocks: TranscriptBlock[] = [];
	private starts: number[] = [];
	private notice = "";
	private cacheKey = "";
	private top = 0;
	private follow = true;
	private searching = false;
	private query = "";
	private matches: number[] = [];
	private current = -1;

	constructor(options: TranscriptViewOptions) {
		this.options = options;
	}

	private height(): number {
		return Math.max(3, this.options.tui.terminal.rows);
	}

	private bodyHeight(): number {
		return this.height() - 2;
	}

	private refresh(width: number): void {
		const entries = this.options.entries();
		const last = entries.at(-1);
		const key = `${width}:${entries.length}:${last?.id ?? ""}`;
		if (key === this.cacheKey) return;
		this.cacheKey = key;
		this.blocks = transcriptBlocks(entries);
		this.starts = [];
		this.lines = this.blocks.length > 0 ? renderBlocks(this.blocks, this.options.theme, width, this.starts) : [`   ${this.options.theme.fg("muted", EMPTY_TRANSCRIPT)}`];
		this.plainLines = this.lines.map((line) => plain(line).toLowerCase());
		if (this.query) this.computeMatches();
	}

	private maxTop(): number {
		return Math.max(0, this.lines.length - this.bodyHeight());
	}

	private scrollTo(top: number): void {
		this.top = Math.min(this.maxTop(), Math.max(0, top));
		this.follow = this.top >= this.maxTop();
	}

	private computeMatches(): void {
		const needle = this.query.toLowerCase();
		this.matches = needle ? this.plainLines.flatMap((line, index) => (line.includes(needle) ? [index] : [])) : [];
		if (this.current >= this.matches.length) this.current = this.matches.length - 1;
	}

	private jump(direction: 1 | -1): void {
		if (this.matches.length === 0) return;
		if (this.current === -1) {
			const index = direction === 1
				? this.matches.findIndex((line) => line >= this.top)
				: this.matches.findLastIndex((line) => line < this.top + this.bodyHeight());
			this.current = index === -1 ? (direction === 1 ? 0 : this.matches.length - 1) : index;
		} else {
			this.current = (this.current + direction + this.matches.length) % this.matches.length;
		}
		const line = this.matches[this.current]!;
		if (line < this.top || line >= this.top + this.bodyHeight()) this.scrollTo(line - Math.floor(this.bodyHeight() / 3));
	}

	private selected(): number {
		const line = this.current >= 0 ? this.matches[this.current]! : this.top;
		return this.starts.findLastIndex((start) => start <= line);
	}

	private copy(text: string, label: string): void {
		this.options.onCopy(text).then(
			() => { this.notice = `Copied ${label}`; },
			(error: unknown) => { this.notice = `Copy failed: ${error instanceof Error ? error.message : String(error)}`; },
		).finally(() => this.options.tui.requestRender());
	}

	render(width: number): string[] {
		const theme = this.options.theme;
		this.refresh(width);
		if (this.follow) this.top = this.maxTop();
		this.top = Math.min(this.top, this.maxTop());
		const selectedStart = this.starts[this.selected()];
		const body = this.bodyHeight();
		const currentLine = this.current >= 0 ? this.matches[this.current] : undefined;
		const matchSet = new Set(this.matches);
		const visible: string[] = [];
		for (let row = 0; row < body; row += 1) {
			const index = this.top + row;
			const line = this.lines[index];
			if (line === undefined) {
				visible.push("");
				continue;
			}
			if (index === currentLine) {
				const text = truncateToWidth(plain(line), width);
				visible.push(theme.bg("selectedBg", `${text}${" ".repeat(Math.max(0, width - visibleWidth(text)))}`));
			}
			else if (matchSet.has(index)) visible.push(truncateToWidth(`${theme.fg("warning", "▌")}${line.slice(1)}`, width));
			else if (index === selectedStart) visible.push(truncateToWidth(`${theme.fg("accent", "▌")}${line.slice(1)}`, width));
			else visible.push(truncateToWidth(line, width));
		}
		return [this.titleBar(width), ...visible, this.statusBar(width)];
	}

	private titleBar(width: number): string {
		const theme = this.options.theme;
		const left = ` ${theme.bold(theme.fg("accent", TRANSCRIPT_TITLE))}`;
		const total = this.lines.length;
		const end = Math.min(total, this.top + this.bodyHeight());
		const percent = total <= this.bodyHeight() ? 100 : Math.round((end / total) * 100);
		const right = theme.fg("muted", `${this.top + 1}–${end} of ${total} · ${percent}% `);
		const gap = Math.max(1, width - visibleWidth(left) - visibleWidth(right));
		return truncateToWidth(`${left}${" ".repeat(gap)}${right}`, width);
	}

	private statusBar(width: number): string {
		const theme = this.options.theme;
		if (this.searching) {
			return truncateToWidth(` ${theme.fg("accent", "/")}${this.query}${theme.fg("accent", "▏")}  ${theme.fg("dim", "enter find · esc cancel")}`, width);
		}
		const found = this.query
			? theme.fg(this.matches.length > 0 ? "text" : "warning", `"${this.query}" ${this.current >= 0 ? `${this.current + 1}/` : ""}${this.matches.length} · `)
			: "";
		if (this.notice) return truncateToWidth(` ${theme.fg("success", this.notice)}`, width);
		const block = this.blocks[this.selected()];
		const copy = block ? `y copy ${block.title} · Y copy all · ` : "";
		const keys = theme.fg("dim", `↑↓ scroll · / search · n/N next/prev · ${copy}e editor · q close`);
		return truncateToWidth(` ${found}${keys}`, width);
	}

	handleInput(data: string): void {
		if (this.searching) {
			this.handleSearchInput(data);
			this.options.tui.requestRender();
			return;
		}
		const page = this.bodyHeight();
		this.notice = "";
		if (data === "q" || matchesKey(data, "escape") || matchesKey(data, "ctrl+o") || matchesKey(data, "ctrl+c")) {
			this.options.onClose();
			return;
		}
		if (data === "j" || matchesKey(data, "down")) this.scrollTo(this.top + 1);
		else if (data === "k" || matchesKey(data, "up")) this.scrollTo(this.top - 1);
		else if (data === " " || data === "f" || matchesKey(data, "pageDown")) this.scrollTo(this.top + page);
		else if (data === "b" || matchesKey(data, "pageUp")) this.scrollTo(this.top - page);
		else if (data === "d" || matchesKey(data, "ctrl+d")) this.scrollTo(this.top + Math.floor(page / 2));
		else if (data === "u" || matchesKey(data, "ctrl+u")) this.scrollTo(this.top - Math.floor(page / 2));
		else if (data === "g" || matchesKey(data, "home")) this.scrollTo(0);
		else if (data === "G" || matchesKey(data, "end")) this.scrollTo(this.maxTop());
		else if (data === "/") {
			this.searching = true;
			this.query = "";
			this.matches = [];
			this.current = -1;
		} else if (data === "y") {
			const index = this.selected();
			const block = transcriptBlocks(this.options.entries(), true)[index];
			const lines = block?.body.split("\n").length ?? 0;
			if (block) this.copy(block.body, `${block.title} (${lines} ${lines === 1 ? "line" : "lines"})`);
		} else if (data === "Y") {
			if (this.blocks.length > 0) this.copy(transcriptMarkdown(this.options.entries()), "the whole transcript");
		} else if (data === "n") this.jump(1);
		else if (data === "N") this.jump(-1);
		else if (data === "e" || data === "v") {
			this.options.onOpenEditor(transcriptMarkdown(this.options.entries()));
			return;
		} else return;
		this.options.tui.requestRender();
	}

	private handleSearchInput(data: string): void {
		if (matchesKey(data, "escape")) {
			this.searching = false;
			this.query = "";
			this.matches = [];
			this.current = -1;
			return;
		}
		if (matchesKey(data, "enter") || matchesKey(data, "return")) {
			this.searching = false;
			this.computeMatches();
			this.current = -1;
			this.jump(1);
			return;
		}
		if (matchesKey(data, "backspace")) {
			this.query = this.query.slice(0, -1);
			return;
		}
		const printable = decodeKittyPrintable(data) ?? (data.length === 1 && data >= " " ? data : undefined);
		if (printable) this.query += printable;
	}

	handleMouse(event: TuiMouseEvent): TuiMouseEventResult | undefined {
		if (event.type !== "wheel" || event.wheelDelta === undefined) return { handled: true, render: false };
		this.scrollTo(this.top + event.wheelDelta);
		return { handled: true };
	}

	invalidate(): void {
		this.cacheKey = "";
	}
}

interface TranscriptHost {
	sessionManager: { getBranch(): SessionEntry[] };
	settingsManager: { getExternalEditorCommand(): string };
	showExtensionCustom<T>(
		factory: (tui: TUI, theme: Theme, keybindings: unknown, done: (result: T) => void) => Component,
		options: { overlay: boolean; overlayOptions: Record<string, unknown> },
	): Promise<T>;
}

interface TranscriptPrototype {
	toggleToolOutputExpansion(this: TranscriptHost): void;
}

const open = new WeakSet<object>();

export function hasConversation(entries: readonly SessionEntry[]): boolean {
	return entries.some((entry) => entry.type === "message");
}

export async function openTranscript(host: TranscriptHost): Promise<void> {
	if (open.has(host)) return;
	open.add(host);
	try {
		await host.showExtensionCustom<void>((tui, theme, _keybindings, done) => {
			const view = new TranscriptView({
				entries: () => host.sessionManager.getBranch(),
				theme,
				tui,
				onClose: () => done(undefined),
				onCopy: copyToClipboard,
				onOpenEditor: (markdown) => {
					tui.stop();
					void editInExternalEditor({ command: host.settingsManager.getExternalEditorCommand(), content: markdown })
						.finally(() => {
							tui.start();
							tui.requestRender(true);
						});
				},
			});
			return view;
		}, {
			overlay: true,
			overlayOptions: { width: "100%", maxHeight: "100%", anchor: "top-left", margin: 0 },
		});
	} finally {
		open.delete(host);
	}
}

export function applyTranscriptUi(): void {
	const prototype = InteractiveMode.prototype as unknown as TranscriptPrototype;
	const toggle = prototype.toggleToolOutputExpansion;
	if (typeof toggle !== "function") {
		throw new Error("pi no longer exposes toggleToolOutputExpansion, so Ctrl+O cannot open the transcript");
	}
	prototype.toggleToolOutputExpansion = function (this: TranscriptHost) {
		if (!hasConversation(this.sessionManager.getBranch())) {
			toggle.call(this);
			return;
		}
		void openTranscript(this);
	};
}
