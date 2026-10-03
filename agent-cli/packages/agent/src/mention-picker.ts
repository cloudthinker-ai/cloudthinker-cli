import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import {
	Editor,
	fuzzyMatch,
	isKeyRelease,
	matchesKey,
	truncateToWidth,
	type AutocompleteItem,
	type AutocompleteProvider,
	type AutocompleteSuggestions,
	type SelectListTheme,
} from "@earendil-works/pi-tui";

import { connectionMentions, type ConnectionMention } from "@cloudthinker/cloud/src/connection-mentions.ts";

export const MENTION_FILTERS = ["All", "Files", "Connections", "Agents"] as const;
const FILTER_KINDS = [undefined, "file", "connection", "agent"] as const;
const KIND_LABEL = { file: "File", connection: "Connection", agent: "Agent" } as const;
const MENTION_TOKEN = /(^|[\s。、？！])([@#])([^\s"'=]*)$/;
const HINT = "←/→ filter · tab insert · esc close";
const CONNECTION_HINT = "tab insert · esc close";
const CONNECTIONS_FILTER = 2;
const EXPLICIT_PATH = /^(\.{1,2}\/|\/|~)/;
const EDITS_WITHOUT_AUTOCOMPLETE = ["deleteToStartOfLine", "deleteToEndOfLine", "deleteWordBackwards", "deleteWordForward", "insertYankedText", "deleteYankedText", "undo", "navigateHistory"] as const;

type Kind = keyof typeof KIND_LABEL;
type MentionItem = AutocompleteItem & { kind?: Kind | "empty" };

interface PickerEditor {
	theme: { selectList: SelectListTheme };
	autocompletePrefix: string;
	isShowingAutocomplete(): boolean;
	updateAutocomplete(): void;
}

interface ListLike {
	render(width: number): string[];
	handleMouse?(event: { y: number; height: number }): unknown;
}

export const picker: { filter: number; anchor: string | undefined; editor: PickerEditor | undefined } = { filter: 0, anchor: undefined, editor: undefined };

function kindOf(item: MentionItem): Kind {
	return item.kind === "agent" || item.kind === "connection" ? item.kind : "file";
}

export function connectionItems(query: string, mentions: ConnectionMention[] = connectionMentions()): MentionItem[] {
	const lower = query.toLowerCase();
	return mentions
		.filter((mention) => !lower || `${mention.prefix} ${mention.alias}`.toLowerCase().includes(lower) || fuzzyMatch(query, `${mention.prefix} ${mention.alias}`).matches)
		.map((mention) => ({
			value: mention.token,
			label: `#${mention.token.slice("#connection/".length)}`,
			description: [mention.alias, mention.description].filter(Boolean).join(" · "),
			kind: "connection" as const,
		}));
}

export function relativeFile(value: string): string {
	const quoted = value.startsWith('@"');
	const path = value.slice(quoted ? 2 : 1);
	if (EXPLICIT_PATH.test(path)) return value;
	return `${quoted ? '@"' : "@"}./${path}`;
}

export function pickMentions(query: string, inner: MentionItem[], connections: MentionItem[], filter: number): MentionItem[] {
	const agents = inner.filter((item) => kindOf(item) === "agent");
	const files = inner.filter((item) => kindOf(item) === "file").map((item) => ({ ...item, value: relativeFile(item.value) }));
	const all = [...agents, ...connections, ...files];
	const kind = FILTER_KINDS[filter];
	const shown = kind ? all.filter((item) => kindOf(item) === kind) : all;
	if (all.length === 0) return [];
	if (shown.length === 0) return [{ value: `@${query}`, label: `No ${MENTION_FILTERS[filter]!.toLowerCase()}${query ? " match" : ""}`, kind: "empty" }];
	return shown.map((item) => ({ ...item, description: `${KIND_LABEL[kindOf(item)]}${item.description ? ` · ${item.description}` : ""}` }));
}

export function createMentionPicker(current: AutocompleteProvider): AutocompleteProvider {
	return {
		triggerCharacters: ["@", "#"],
		async getSuggestions(lines, cursorLine, cursorCol, options): Promise<AutocompleteSuggestions | null> {
			const match = MENTION_TOKEN.exec((lines[cursorLine] ?? "").slice(0, cursorCol));
			if (!match) return current.getSuggestions(lines, cursorLine, cursorCol, options);
			const sigil = match[2]!;
			const query = match[3]!;
			const anchor = `${cursorLine}:${cursorCol - query.length}`;
			if (anchor !== picker.anchor) {
				picker.anchor = anchor;
				picker.filter = sigil === "#" ? CONNECTIONS_FILTER : 0;
			}
			if (sigil === "#") {
				const items = connectionItems(query.replace(/^connection\/?/, ""));
				return items.length > 0 ? { items: pickMentions(query, [], items, CONNECTIONS_FILTER), prefix: `#${query}` } : null;
			}
			let theirs: AutocompleteSuggestions | null = null;
			try {
				theirs = await current.getSuggestions(lines, cursorLine, cursorCol, options);
			} catch {
				theirs = null;
			}
			const prefix = `@${query}`;
			const inner = theirs && theirs.prefix === prefix ? (theirs.items as MentionItem[]) : [];
			const items = pickMentions(query, inner, connectionItems(query), picker.filter);
			return items.length > 0 ? { items, prefix } : null;
		},
		applyCompletion(lines, cursorLine, cursorCol, item, prefix) {
			const kind = (item as MentionItem).kind;
			if (kind === "empty") return { lines, cursorLine, cursorCol };
			if (kind === "connection") {
				const line = lines[cursorLine] ?? "";
				const start = cursorCol - prefix.length;
				const after = line.slice(cursorCol).replace(/^ /, "");
				const next = [...lines];
				next[cursorLine] = `${line.slice(0, start)}${item.value} ${after}`;
				return { lines: next, cursorLine, cursorCol: start + item.value.length + 1 };
			}
			return current.applyCompletion(lines, cursorLine, cursorCol, item, prefix);
		},
		shouldTriggerFileCompletion(lines, cursorLine, cursorCol) {
			return current.shouldTriggerFileCompletion?.(lines, cursorLine, cursorCol) ?? true;
		},
	};
}

export function filterTabs(theme: SelectListTheme, filter: number, width: number): string {
	const tabs = MENTION_FILTERS.map((name, index) => (index === filter ? theme.selectedText(` ${name} `) : theme.description(` ${name} `)));
	return truncateToWidth(`  ${tabs.join(" ")}`, width);
}

export function applyMentionPicker(): void {
	const prototype = Editor.prototype as unknown as {
		createAutocompleteList(this: PickerEditor, prefix: string, items: AutocompleteItem[]): ListLike;
		clearAutocompleteUi(this: PickerEditor): void;
		updateAutocomplete?: unknown;
	} & Record<(typeof EDITS_WITHOUT_AUTOCOMPLETE)[number], ((this: PickerEditor, ...args: unknown[]) => unknown) | undefined>;
	const create = prototype.createAutocompleteList;
	const clear = prototype.clearAutocompleteUi;
	if (typeof create !== "function" || typeof clear !== "function" || typeof prototype.updateAutocomplete !== "function") {
		throw new Error("pi-tui's editor autocomplete seam changed, so @ cannot show its mention filters");
	}
	for (const name of EDITS_WITHOUT_AUTOCOMPLETE) {
		const edit = prototype[name];
		if (typeof edit !== "function") throw new Error(`pi-tui's editor no longer has ${name}, so a stale @ list could outlive its token`);
		prototype[name] = function (...args) {
			const result = edit.apply(this, args);
			if (this.isShowingAutocomplete()) this.updateAutocomplete();
			return result;
		};
	}
	prototype.createAutocompleteList = function (prefix, items) {
		const list = create.call(this, prefix, items);
		if (!prefix.startsWith("@") && !prefix.startsWith("#")) return list;
		picker.editor = this;
		const theme = this.theme.selectList;
		const render = list.render.bind(list);
		const tabs = prefix.startsWith("@");
		list.render = (width) => [...(tabs ? [filterTabs(theme, picker.filter, width)] : []), ...render(width), truncateToWidth(theme.description(`  ${tabs ? HINT : CONNECTION_HINT}`), width)];
		const mouse = list.handleMouse?.bind(list);
		if (mouse && tabs) list.handleMouse = (event) => (event.y < 1 ? { handled: true } : mouse({ ...event, y: event.y - 1, height: event.height - 2 }));
		return list;
	};
	prototype.clearAutocompleteUi = function () {
		if (picker.editor === this) picker.editor = undefined;
		clear.call(this);
	};
}

export function stepFilter(data: string): boolean {
	const editor = picker.editor;
	if (!editor || isKeyRelease(data) || !editor.isShowingAutocomplete() || !editor.autocompletePrefix.startsWith("@")) return false;
	const step = matchesKey(data, "left") ? -1 : matchesKey(data, "right") ? 1 : 0;
	if (step === 0) return false;
	picker.filter = (picker.filter + step + MENTION_FILTERS.length) % MENTION_FILTERS.length;
	editor.updateAutocomplete();
	return true;
}

export function registerMentionPicker(pi: ExtensionAPI): void {
	let registered = false;
	pi.on("session_start", (_event, ctx) => {
		if (ctx.mode !== "tui" || !ctx.hasUI || registered) return;
		registered = true;
		ctx.ui.addAutocompleteProvider((current) => createMentionPicker(current));
		ctx.ui.onTerminalInput((data) => (stepFilter(data) ? { consume: true } : undefined));
	});
}
