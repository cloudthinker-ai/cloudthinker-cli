import { InteractiveMode } from "@earendil-works/pi-coding-agent";

import { PRODUCT_NAME } from "@cloudthinker/pi/src/header.ts";

import { BUILTIN_SLASH_COMMANDS } from "../node_modules/@earendil-works/pi-coding-agent/dist/core/slash-commands.js";

export const NO_SESSION_FLAG = "--no-session";
export const NO_SESSION_REFUSAL =
	"cloudthinker agent always keeps a session; --no-session is not supported";
export const DISABLED_COMMANDS: readonly string[] = ["share", "thinking"];
export const SHARE_STATUS =
	"/share is disabled: this session already mirrors to your CloudThinker workspace, and a gist would publish its cloud output.";
export const THINKING_STATUS =
	"/thinking is disabled: the agent mode you pick with /model carries its own thinking level.";
export const SESSION_COMMAND = "/session";
export const SESSION_REPLACEMENT = `/${PRODUCT_NAME} session`;

export type SubmitHandler = (text: string) => unknown;

export interface SubmitHost {
	defaultEditor: { onSubmit?: SubmitHandler };
	editor: { setText: (text: string) => void };
	showStatus: (message: string) => void;
}

interface SubmitHostPrototype {
	setupEditorSubmitHandler?: (this: SubmitHost) => void;
}

export function hasNoSessionFlag(argv: string[]): boolean {
	const separator = argv.indexOf("--");
	const flags = separator === -1 ? argv : argv.slice(0, separator);
	return flags.includes(NO_SESSION_FLAG);
}

export function rewrittenCommand(text: string): string | undefined {
	return text.trim() === SESSION_COMMAND ? SESSION_REPLACEMENT : undefined;
}

export function disabledCommandStatus(text: string): string | undefined {
	const trimmed = text.trim();
	if (trimmed === "/share") return SHARE_STATUS;
	if (trimmed === "/thinking" || trimmed.startsWith("/thinking ")) return THINKING_STATUS;
	return undefined;
}

export function guardedSubmit(
	host: SubmitHost,
	original: SubmitHandler | undefined,
): SubmitHandler {
	return (text) => {
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
			"pi's InteractiveMode no longer defines setupEditorSubmitHandler, so /share and /thinking cannot be disabled",
		);
	}
	prototype.setupEditorSubmitHandler = function (this: SubmitHost) {
		original.call(this);
		this.defaultEditor.onSubmit = guardedSubmit(this, this.defaultEditor.onSubmit);
	};
}

export function removeDisabledCommands(commands: { name: string }[]): string[] {
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

export function interactiveModePrototype(): SubmitHostPrototype {
	return InteractiveMode.prototype as unknown as SubmitHostPrototype;
}

export function builtinSlashCommands(): { name: string }[] {
	return BUILTIN_SLASH_COMMANDS as unknown as { name: string }[];
}

export function applyGuard(): void {
	wrapSubmitHandler(interactiveModePrototype());
	const removed = removeDisabledCommands(builtinSlashCommands());
	if (removed.length !== DISABLED_COMMANDS.length) {
		throw new Error(
			`pi's BUILTIN_SLASH_COMMANDS no longer offers ${DISABLED_COMMANDS.join(" and ")}, so the guard removed only ${removed.length}`,
		);
	}
}
