import { createReadStream } from "node:fs";
import { readdir, stat } from "node:fs/promises";
import { createInterface } from "node:readline";
import { join } from "node:path";

import { InteractiveMode, type Theme } from "@earendil-works/pi-coding-agent";
import { type Component, decodeKittyPrintable, matchesKey, type TUI, truncateToWidth } from "@earendil-works/pi-tui";

import { sanitizeTerminalText } from "@cloudthinker/cloud/src/awareness.ts";

import { KEYBINDINGS } from "../node_modules/@earendil-works/pi-coding-agent/dist/core/keybindings.js";

export const PROMPT_SEARCH = "app.prompt.search";
export const PROMPT_SEARCH_KEY = "ctrl+r";
export const MAX_SEARCH_SESSIONS = 30;
export const SEARCH_ROWS = 8;
export const MAX_SESSION_BYTES = 32 * 1024 * 1024;

const cache = new Map<string, { modified: number; prompts: string[] }>();

interface PromptSearchHost {
	editor: { history?: string[]; getText(): string; setText(text: string): void };
	sessionManager: { getSessionDir(): string; getSessionFile(): string | undefined };
	ui: { requestRender(): void };
	showExtensionCustom<T>(
		factory: (tui: TUI, theme: Theme, keybindings: unknown, done: (result: T) => void) => Component,
		options: { overlay: boolean; overlayOptions: Record<string, unknown> },
	): Promise<T>;
}

async function userPrompts(path: string): Promise<string[]> {
	const prompts: string[] = [];
	const lines = createInterface({ input: createReadStream(path, "utf8"), crlfDelay: Infinity });
	for await (const line of lines) {
		if (!line.includes('"role":"user"')) continue;
		try {
			const content = (JSON.parse(line) as { message?: { role?: string; content?: unknown } }).message;
			if (content?.role !== "user") continue;
			const text = typeof content.content === "string"
				? content.content
				: Array.isArray(content.content)
					? content.content.map((part: { type?: string; text?: string }) => (part.type === "text" ? (part.text ?? "") : "")).join("\n")
					: "";
			if (text.trim()) prompts.push(text.trim());
		} catch {
			continue;
		}
	}
	return prompts;
}

export async function pastPrompts(sessionDir: string, current: readonly string[], skipFile?: string): Promise<string[]> {
	const files = await readdir(sessionDir).catch(() => [] as string[]);
	const dated = await Promise.all(files.filter((name) => name.endsWith(".jsonl")).map(async (name) => {
		const path = join(sessionDir, name);
		const info = await stat(path).catch(() => undefined);
		return { path, modified: info?.mtimeMs ?? 0, size: info?.size ?? 0 };
	}));
	const recent = dated
		.filter((file) => file.path !== skipFile && file.size <= MAX_SESSION_BYTES)
		.sort((left, right) => right.modified - left.modified)
		.slice(0, MAX_SEARCH_SESSIONS);
	const older = await Promise.all(recent.map(async (file) => {
		const cached = cache.get(file.path);
		if (cached?.modified === file.modified) return cached.prompts;
		const prompts = (await userPrompts(file.path).catch(() => [] as string[])).reverse();
		cache.set(file.path, { modified: file.modified, prompts });
		return prompts;
	}));
	return [...new Set([...current, ...older.flat()])];
}

export function matchPrompts(prompts: readonly string[], query: string): string[] {
	const needles = query.toLowerCase().split(/\s+/).filter(Boolean);
	return prompts.filter((prompt) => {
		const haystack = prompt.toLowerCase();
		return needles.every((needle) => haystack.includes(needle));
	});
}

export class PromptSearch implements Component {
	private readonly prompts: readonly string[];
	private readonly theme: Theme;
	private readonly done: (prompt: string | undefined) => void;
	private query = "";
	private selected = 0;

	constructor(prompts: readonly string[], theme: Theme, done: (prompt: string | undefined) => void, query = "") {
		this.prompts = prompts;
		this.theme = theme;
		this.done = done;
		this.query = query;
	}

	private matches(): string[] {
		return matchPrompts(this.prompts, this.query);
	}

	render(width: number): string[] {
		const theme = this.theme;
		const matches = this.matches();
		this.selected = Math.min(this.selected, Math.max(0, matches.length - 1));
		const first = Math.max(0, this.selected - SEARCH_ROWS + 1);
		const rows = matches.slice(first, first + SEARCH_ROWS).map((prompt, offset) => {
			const active = first + offset === this.selected;
			const text = sanitizeTerminalText(prompt.replace(/\s*\n\s*/g, " ⏎ "));
			return truncateToWidth(active ? `${theme.fg("accent", "›")} ${theme.fg("accent", text)}` : `  ${theme.fg("text", text)}`, width);
		}).reverse();
		const count = this.prompts.length === 0 ? "no past prompts" : `${matches.length} of ${this.prompts.length}`;
		return [
			theme.fg("border", "─".repeat(width)),
			...(rows.length > 0 ? rows : [`  ${theme.fg("muted", this.query ? "No prompt matches." : "No past prompts yet.")}`]),
			truncateToWidth(` ${theme.fg("accent", "search prompts:")} ${this.query}${theme.fg("accent", "▏")}  ${theme.fg("dim", `${count} · ↑↓ older/newer · enter edit · esc cancel`)}`, width),
			theme.fg("border", "─".repeat(width)),
		];
	}

	handleInput(data: string): void {
		if (matchesKey(data, "escape") || matchesKey(data, "ctrl+c")) return this.done(undefined);
		if (matchesKey(data, "enter") || matchesKey(data, "return")) return this.done(this.matches()[this.selected]);
		if (matchesKey(data, "up") || matchesKey(data, "ctrl+r")) this.selected = Math.min(this.selected + 1, Math.max(0, this.matches().length - 1));
		else if (matchesKey(data, "down")) this.selected = Math.max(0, this.selected - 1);
		else if (matchesKey(data, "backspace")) this.query = this.query.slice(0, -1);
		else {
			const printable = decodeKittyPrintable(data) ?? (data.length === 1 && data >= " " ? data : undefined);
			if (!printable) return;
			this.query += printable;
			this.selected = 0;
		}
	}

	invalidate(): void {}
}

const searching = new WeakSet<object>();

export async function openPromptSearch(host: PromptSearchHost): Promise<void> {
	if (searching.has(host)) return;
	searching.add(host);
	try {
		await showPromptSearch(host);
	} finally {
		searching.delete(host);
	}
}

async function showPromptSearch(host: PromptSearchHost): Promise<void> {
	const prompts = await pastPrompts(host.sessionManager.getSessionDir(), host.editor.history ?? [], host.sessionManager.getSessionFile());
	const draft = host.editor.getText();
	const query = draft.includes("\n") ? "" : draft.trim();
	const choice = await host.showExtensionCustom<string | undefined>((tui, theme, _keybindings, done) => {
		const view = new PromptSearch(prompts, theme, done, query);
		const handleInput = view.handleInput.bind(view);
		view.handleInput = (data) => {
			handleInput(data);
			tui.requestRender();
		};
		return view;
	}, { overlay: true, overlayOptions: { width: "100%", anchor: "bottom-left", margin: 0 } });
	if (choice !== undefined) host.editor.setText(choice);
	host.ui.requestRender();
}

export function applyPromptSearchKey(): void {
	const definitions = KEYBINDINGS as unknown as Record<string, { defaultKeys: string | string[]; description?: string }>;
	if (PROMPT_SEARCH in definitions) throw new Error(`pi already defines ${PROMPT_SEARCH}, so Ctrl+R prompt search would collide`);
	const prototype = InteractiveMode.prototype as unknown as {
		setupKeyHandlers(this: PromptSearchHost & { defaultEditor: { onAction(action: string, handler: () => void): void } }): void;
	};
	const setupKeyHandlers = prototype.setupKeyHandlers;
	if (typeof setupKeyHandlers !== "function") {
		throw new Error("pi's InteractiveMode no longer defines setupKeyHandlers, so Ctrl+R cannot search prompts");
	}
	definitions[PROMPT_SEARCH] = { defaultKeys: PROMPT_SEARCH_KEY, description: "Search past prompts" };
	prototype.setupKeyHandlers = function () {
		setupKeyHandlers.call(this);
		this.defaultEditor.onAction(PROMPT_SEARCH, () => void openPromptSearch(this));
	};
}
