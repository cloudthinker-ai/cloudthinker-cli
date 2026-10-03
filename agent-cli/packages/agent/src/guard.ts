import { InteractiveMode } from "@earendil-works/pi-coding-agent";

import { PRODUCT_NAME } from "@cloudthinker/cloud/src/header.ts";

import { BUILTIN_SLASH_COMMANDS } from "../node_modules/@earendil-works/pi-coding-agent/dist/core/slash-commands.js";
import { applyAwarenessUi } from "./awareness.ts";
import { applyClearScreenKey } from "./clear-screen.ts";
import { applyFilePicker } from "./file-picker.ts";
import { applyMentionPicker } from "./mention-picker.ts";
import { applyMentionHighlight } from "./mention-highlight.ts";
import { applyCompletionRows } from "./background/index.ts";
import { applyPromptSearchKey } from "./prompt-search.ts";
import { applyCodeBlockNumbers, COPY_USAGE, copyCodeBlock, copyCommandIndex, type CopyHost } from "./code-blocks.ts";
import { applyMermaidUi } from "./mermaid.ts";
import { applyCloudModelGuidance, applyCloudOnlyModels } from "./models.ts";
import { applyQueuedMessagesUi } from "./queue-ui.ts";
import { applyReasoningUiGuard } from "./reasoning-ui.ts";
import { applySkillCatalogGuard } from "./skill-catalog.ts";
import { applyScrollPill } from "./scroll-pill.ts";
import { applyStartupUi } from "./startup.ts";
import { applyThinkingUi } from "./thinking-ui.ts";
import { applyToolGroups } from "./tool-groups.ts";
import { applyTranscriptUi } from "./transcript.ts";

export const DISABLED_COMMANDS: readonly string[] = ["thinking", "scoped-models"];
export const THINKING_STATUS =
	"/thinking is disabled: the agent mode you pick with /model carries its own thinking level.";
export const SCOPED_MODELS_STATUS = "/scoped-models is disabled: /model lists your CloudThinker agent modes.";
export const CLOUD_COMMANDS = [
	{ name: "share", description: "Copy this conversation's CloudThinker link" },
	{ name: "login", description: "Sign in to CloudThinker" },
	{ name: "logout", description: "Sign out of CloudThinker" },
	{ name: "changelog", description: "Show what's new in this CloudThinker agent" },
	{ name: "bug", description: "Report a bug on the CloudThinker CLI GitHub" },
] as const;
export const COPY_DESCRIPTION = "Copy the last answer, or its code block N (/copy 2)";
export const SESSION_COMMAND = "/session";
export const SESSION_REPLACEMENT = `/${PRODUCT_NAME} session`;
export const COMMAND_ALIASES = [
	{ alias: "exit", target: "quit" },
	{ alias: "clear", target: "new" },
	{ alias: "config", target: "settings" },
] as const;

export interface SlashCommand {
	name: string;
	description?: string;
	argumentHint?: string;
}

export type SubmitHandler = (text: string) => unknown;

export interface SubmitHost {
	defaultEditor: { onSubmit?: SubmitHandler };
	editor: { setText: (text: string) => void };
	showStatus: (message: string) => void;
}

interface SubmitHostPrototype {
	setupEditorSubmitHandler?: (this: SubmitHost) => void;
}

export function rewrittenCommand(text: string): string | undefined {
	const trimmed = text.trim();
	if (trimmed === SESSION_COMMAND) return SESSION_REPLACEMENT;
	const [head = "", ...rest] = trimmed.split(/\s+/);
	const cloud = CLOUD_COMMANDS.find((command) => head === `/${command.name}`);
	if (cloud) return [`/${PRODUCT_NAME}`, cloud.name, ...(cloud.name === "bug" ? rest : [])].join(" ");
	const alias = COMMAND_ALIASES.find((entry) => trimmed === `/${entry.alias}`);
	return alias ? `/${alias.target}` : undefined;
}

export function disabledCommandStatus(text: string): string | undefined {
	const trimmed = text.trim();
	if (trimmed === "/thinking" || trimmed.startsWith("/thinking ")) return THINKING_STATUS;
	if (trimmed === "/scoped-models") return SCOPED_MODELS_STATUS;
	return undefined;
}

export function guardedSubmit(
	host: SubmitHost,
	original: SubmitHandler | undefined,
): SubmitHandler {
	return (text) => {
		const copy = copyCommandIndex(text);
		if (copy !== undefined) {
			host.editor.setText("");
			if (copy === "usage") host.showStatus(COPY_USAGE);
			else void copyCodeBlock(host as unknown as CopyHost, copy);
			return undefined;
		}
		const status = disabledCommandStatus(text);
		if (status !== undefined) {
			host.showStatus(status);
			host.editor.setText("");
			return undefined;
		}
		const rewritten = rewrittenCommand(text);
		if (rewritten !== undefined) {
			host.editor.setText("");
			return original?.(rewritten);
		}
		return original?.(text);
	};
}

export function wrapSubmitHandler(prototype: SubmitHostPrototype): void {
	const original = prototype.setupEditorSubmitHandler;
	if (!original) {
		throw new Error(
			"pi's InteractiveMode no longer defines setupEditorSubmitHandler, so /thinking stays live and /share, /login, and /bug reach pi's own handlers",
		);
	}
	prototype.setupEditorSubmitHandler = function (this: SubmitHost) {
		original.call(this);
		this.defaultEditor.onSubmit = guardedSubmit(this, this.defaultEditor.onSubmit);
	};
}

export function removeDisabledCommands(commands: SlashCommand[]): string[] {
	const removed: string[] = [];
	for (let index = commands.length - 1; index >= 0; index -= 1) {
		const command = commands[index];
		if (command && DISABLED_COMMANDS.includes(command.name)) {
			commands.splice(index, 1);
			removed.push(command.name);
		}
	}
	return removed;
}

export function registerCommandAliases(commands: SlashCommand[]): void {
	const byName = new Map(commands.map((command) => [command.name, command]));
	for (const alias of COMMAND_ALIASES) {
		const target = byName.get(alias.target);
		if (!target) {
			throw new Error(`pi no longer offers /${alias.target}, so /${alias.alias} cannot be registered`);
		}
		if (!byName.has(alias.alias)) {
			const command = { name: alias.alias, description: `${target.description ?? `Run /${alias.target}`} (alias for /${alias.target})` };
			commands.push(command);
			byName.set(alias.alias, command);
		}
	}
}

export function describeCloudCommands(commands: SlashCommand[]): void {
	for (const replacement of CLOUD_COMMANDS) {
		const command = commands.find((candidate) => candidate.name === replacement.name);
		if (!command) throw new Error(`pi no longer offers /${replacement.name}, so its CloudThinker replacement has no picker entry`);
		command.description = replacement.description;
		if (replacement.name === "bug") command.argumentHint = "[title]";
		else delete command.argumentHint;
	}
	const model = commands.find((candidate) => candidate.name === "model");
	if (!model) throw new Error("pi no longer offers /model, so the agent mode picker has no entry");
	model.description = "Pick an agent mode: light, pro, or ultra";
	model.argumentHint = "[mode]";
}

export function interactiveModePrototype(): SubmitHostPrototype {
	return InteractiveMode.prototype as unknown as SubmitHostPrototype;
}

export function builtinSlashCommands(): SlashCommand[] {
	return BUILTIN_SLASH_COMMANDS as unknown as SlashCommand[];
}

export function applyGuard(): void {
	applyMermaidUi();
	wrapSubmitHandler(interactiveModePrototype());
	const removed = removeDisabledCommands(builtinSlashCommands());
	if (removed.length !== DISABLED_COMMANDS.length) {
		throw new Error(
			`pi's BUILTIN_SLASH_COMMANDS no longer offers ${DISABLED_COMMANDS.join(" and ")}, so the guard removed only ${removed.length}`,
		);
	}
	registerCommandAliases(builtinSlashCommands());
	describeCloudCommands(builtinSlashCommands());
	const copy = builtinSlashCommands().find((command) => command.name === "copy");
	if (!copy) throw new Error("pi's BUILTIN_SLASH_COMMANDS no longer offers /copy, so /copy N cannot be described");
	copy.description = COPY_DESCRIPTION;
	copy.argumentHint = "[N]";
	applyReasoningUiGuard();
	applyThinkingUi();
	applyAwarenessUi();
	applyStartupUi();
	applySkillCatalogGuard();
	applyTranscriptUi();
	applyQueuedMessagesUi();
	applyScrollPill();
	applyClearScreenKey();
	applyCodeBlockNumbers();
	applyPromptSearchKey();
	applyFilePicker();
	applyMentionPicker();
	applyMentionHighlight();
	applyCompletionRows();
	applyToolGroups();
	applyCloudOnlyModels();
	applyCloudModelGuidance();
}
