import { mkdirSync, readFileSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

import { type ExtensionAPI, type ExtensionContext, SettingsManager, getAgentDir } from "@earendil-works/pi-coding-agent";
import { Container, truncateToWidth } from "@earendil-works/pi-tui";
import { type Static, Type } from "typebox";

import { terminalNotify } from "../notify.ts";
import { ApprovalPrompt } from "./approval-prompt.ts";
import { text } from "./shared.ts";

export const CT_CONFIG = "ct_config";
export const SAVE_SETTING = "Save";
export const DECLINE_SETTING = "Decline";
export const SETTING_APPROVAL_TITLE = "change a setting";
export const SETTING_DECLINED = "The user declined this settings change. Ask how to proceed instead of retrying it.";
export const SETTING_TAKES_EFFECT = "It takes effect when the user restarts the agent.";

type SettingValue = boolean | string;
type SettingsObject = Record<string, unknown>;

export interface SettingKey {
	key: string;
	values: readonly SettingValue[];
	fallback: SettingValue;
	description: string;
}

const BOOLEAN = [true, false] as const;

export const SETTING_KEYS: readonly SettingKey[] = [
	{ key: "cloudDefault", values: BOOLEAN, fallback: true, description: "Start new sessions with Cloud on." },
	{ key: "toolOutput", values: ["compact", "preview"], fallback: "compact", description: "Show tool results as a one-line summary or a short preview." },
	{ key: "diffStyle", values: ["auto", "unified", "split"], fallback: "auto", description: "Draw file changes stacked, side by side, or by terminal width." },
	{ key: "hideThinkingBlock", values: BOOLEAN, fallback: true, description: "Hide the agent's thinking in the chat (Ctrl+T shows it)." },
	{ key: "markdown.mermaid", values: ["off", "final", "streaming"], fallback: "streaming", description: "Draw Mermaid diagrams never, after the answer, or while it streams." },
	{ key: "markdown.mermaidBorderColors", values: BOOLEAN, fallback: false, description: "Keep the border colors a Mermaid diagram sets." },
	{ key: "fullscreenCopyOnSelect", values: BOOLEAN, fallback: true, description: "Copy text to the clipboard when you select it." },
	{ key: "terminal.showImages", values: BOOLEAN, fallback: true, description: "Show images in the terminal when it supports them." },
];

const parameters = Type.Object({
	action: Type.Union([Type.Literal("list"), Type.Literal("get"), Type.Literal("set"), Type.Literal("undo")], {
		description: "list: every setting you can change. get: one setting. set: change one after the user approves. undo: restore the value before the last set, after the user approves.",
	}),
	key: Type.Optional(Type.String({ description: "Setting name from list, for get and set." })),
	value: Type.Optional(Type.String({ description: "New value as text, for set: true, false, or one of the allowed words." })),
});

const description = [
	"Read or change the user's CloudThinker agent settings: the same settings.json that /settings writes. Change settings with this tool, not by editing settings.json.",
	"Only the settings that list shows exist. Each set and undo shows the user the old and new value and waits for their approval, even in Auto mode.",
	"Login, tokens, API keys and the server address are not settings here; tell the user the command to run instead.",
	SETTING_TAKES_EFFECT,
].join("\n");

interface Backup {
	key: string;
	before: SettingValue | null;
	after: SettingValue | null;
}

export interface ConfigDetails {
	action: string;
	key?: string;
	from?: SettingValue | null;
	to?: SettingValue | null;
}

export function settingsPath(): string {
	return join(getAgentDir(), "settings.json");
}

export function backupPath(): string {
	return join(getAgentDir(), "cloudthinker", "settings-backup.json");
}

function show(value: SettingValue | null | undefined): string {
	return value === null || value === undefined ? "not set" : String(value);
}

function lookup(key: string): SettingKey {
	const found = SETTING_KEYS.find((entry) => entry.key === key);
	if (found) return found;
	throw new Error(`Unknown setting "${key}". Settings you can change: ${SETTING_KEYS.map((entry) => entry.key).join(", ")}.`);
}

function parse(entry: SettingKey, raw: string | undefined): SettingValue {
	const trimmed = raw?.trim() ?? "";
	const value = entry.values.find((allowed) => String(allowed) === trimmed);
	if (value !== undefined) return value;
	throw new Error(`"${trimmed}" is not a valid value for ${entry.key}. Allowed: ${entry.values.join(", ")}.`);
}

function readGlobal(): SettingsObject {
	const path = settingsPath();
	let raw: string;
	try {
		raw = readFileSync(path, "utf8");
	} catch (error) {
		if (error instanceof Error && "code" in error && error.code === "ENOENT") return {};
		throw error;
	}
	if (raw.trim() === "") return {};
	try {
		const parsed: unknown = JSON.parse(raw);
		if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) return parsed as SettingsObject;
	} catch {
		return unreadable(path);
	}
	return unreadable(path);
}

function unreadable(path: string): never {
	throw new Error(`${path} is not a valid JSON object, so no setting was changed. Ask the user to fix or remove that file.`);
}

function valueAt(settings: SettingsObject, key: string): SettingValue | null {
	let node: unknown = settings;
	for (const part of key.split(".")) {
		if (!node || typeof node !== "object") return null;
		node = (node as SettingsObject)[part];
	}
	return typeof node === "boolean" || typeof node === "string" ? node : null;
}

function withValue(settings: SettingsObject, key: string, value: SettingValue | null): SettingsObject {
	const next = structuredClone(settings);
	const parts = key.split(".");
	const last = parts.pop() as string;
	let node = next;
	for (const part of parts) {
		const child = node[part];
		if (!child || typeof child !== "object" || Array.isArray(child)) node[part] = {};
		node = node[part] as SettingsObject;
	}
	if (value === null) delete node[last];
	else node[last] = value;
	return next;
}

function writeAtomic(path: string, body: string): void {
	mkdirSync(dirname(path), { recursive: true });
	const temp = `${path}.${process.pid}.tmp`;
	writeFileSync(temp, body);
	renameSync(temp, path);
}

function readBackup(): Backup | undefined {
	try {
		const parsed = JSON.parse(readFileSync(backupPath(), "utf8")) as Backup;
		return typeof parsed.key === "string" ? parsed : undefined;
	} catch {
		return undefined;
	}
}

function projectShadow(ctx: ExtensionContext, key: string): string {
	const project = SettingsManager.create(ctx.cwd, getAgentDir(), { projectTrusted: ctx.isProjectTrusted() }).getProjectSettings();
	const value = valueAt(project as SettingsObject, key);
	return value === null ? "" : ` This project's .pi/settings.json sets ${key} to ${value}, which wins in this folder.`;
}

function describe(entry: SettingKey, settings: SettingsObject): string {
	const saved = valueAt(settings, entry.key);
	const current = saved === null ? `${entry.fallback} (default)` : String(saved);
	return `${entry.key}: ${current} · allowed: ${entry.values.join(", ")} · ${entry.description}`;
}

async function approve(ctx: ExtensionContext, entry: SettingKey, from: SettingValue | null, to: SettingValue | null): Promise<boolean> {
	const change = `${entry.key}: ${show(from)} → ${show(to)}`;
	if (ctx.mode === "tui") {
		terminalNotify("CloudThinker: approval needed");
		const choice = await ctx.ui.custom<string | undefined>((tui, theme, _keybindings, done) => new ApprovalPrompt({
			title: SETTING_APPROVAL_TITLE,
			reasoning: `The agent wants to change a setting. ${entry.description}`,
			place: "local",
			connections: settingsPath(),
			script: change,
			webUrl: "",
			choices: [{ label: SAVE_SETTING, key: "y" }, { label: DECLINE_SETTING, key: "n" }],
		}, theme, done, () => tui.requestRender(), () => tui.terminal.rows));
		return choice === SAVE_SETTING;
	}
	return await ctx.ui.select(`Change a setting: ${change}`, [SAVE_SETTING, DECLINE_SETTING]) === SAVE_SETTING;
}

export function headlessRefusal(action: string): string {
	return `${CT_CONFIG} ${action} was refused: a settings change needs the user's approval, and this run has no terminal to ask. Tell the user the change, or ask them to run the agent interactively.`;
}

async function change(
	ctx: ExtensionContext,
	action: "set" | "undo",
	entry: SettingKey,
	from: SettingValue | null,
	to: SettingValue | null,
	settings: SettingsObject,
): Promise<void> {
	if (!ctx.hasUI) throw new Error(headlessRefusal(action));
	if (!await approve(ctx, entry, from, to)) throw new Error(SETTING_DECLINED);
	const latest = readGlobal();
	if (valueAt(latest, entry.key) !== valueAt(settings, entry.key)) {
		throw new Error(`${entry.key} changed while the user was deciding, so nothing was saved. Read it again with get.`);
	}
	if (action === "set") writeAtomic(backupPath(), `${JSON.stringify({ key: entry.key, before: from, after: to } satisfies Backup, null, 2)}\n`);
	writeAtomic(settingsPath(), `${JSON.stringify(withValue(latest, entry.key, to), null, 2)}\n`);
	if (action === "undo") rmSync(backupPath(), { force: true });
}

export async function runConfig(params: Static<typeof parameters>, ctx: ExtensionContext): Promise<{ body: string; details: ConfigDetails }> {
	if (params.action === "list") {
		const settings = readGlobal();
		return {
			body: [`Settings in ${settingsPath()}:`, ...SETTING_KEYS.map((entry) => describe(entry, settings))].join("\n"),
			details: { action: "list" },
		};
	}
	if (params.action === "undo") {
		const backup = readBackup();
		if (!backup) throw new Error(`There is no ${CT_CONFIG} change to undo.`);
		const entry = lookup(backup.key);
		const settings = readGlobal();
		const now = valueAt(settings, entry.key);
		if (now !== backup.after) {
			throw new Error(`${entry.key} is now ${show(now)}, not the ${show(backup.after)} that ${CT_CONFIG} saved, so it was changed since. Nothing was restored; use set to change it.`);
		}
		await change(ctx, "undo", entry, now, backup.before, settings);
		return {
			body: `Restored ${entry.key}: ${show(now)} → ${show(backup.before)}. ${SETTING_TAKES_EFFECT}`,
			details: { action: "undo", key: entry.key, from: now, to: backup.before },
		};
	}
	const entry = lookup(params.key?.trim() ?? "");
	if (params.action === "get") {
		return { body: `${describe(entry, readGlobal())}${projectShadow(ctx, entry.key)}`, details: { action: "get", key: entry.key } };
	}
	const to = parse(entry, params.value);
	const settings = readGlobal();
	const from = valueAt(settings, entry.key);
	if (from === to) {
		return { body: `${entry.key} is already ${to}; nothing changed.${projectShadow(ctx, entry.key)}`, details: { action: "set", key: entry.key, from, to } };
	}
	await change(ctx, "set", entry, from, to, settings);
	return {
		body: `Saved ${entry.key}: ${show(from)} → ${to} in ${settingsPath()}. ${SETTING_TAKES_EFFECT} Undo with action undo.${projectShadow(ctx, entry.key)}`,
		details: { action: "set", key: entry.key, from, to },
	};
}

export function registerConfig(pi: ExtensionAPI): void {
	pi.registerTool<typeof parameters, ConfigDetails>({
		name: CT_CONFIG,
		label: "Agent settings",
		description,
		promptSnippet: "Read or change the user's CloudThinker agent settings, with their approval",
		parameters,
		execute: async (_toolCallId, params, _signal, _onUpdate, ctx) => {
			const result = await runConfig(params, ctx);
			return text(result.body, result.details);
		},
		renderCall: (params, theme) => {
			const container = new Container();
			const rest = [params.action, params.key, params.value].filter((part) => part).join(" ");
			const line = `${theme.fg("toolTitle", theme.bold(CT_CONFIG))}  ${theme.fg("muted", rest)}`;
			container.addChild({ render: (width) => [truncateToWidth(line, width, "…")], invalidate: () => {} });
			return container;
		},
	});
}
