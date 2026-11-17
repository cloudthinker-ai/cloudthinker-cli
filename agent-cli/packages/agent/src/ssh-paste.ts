import { randomUUID } from "node:crypto";
import { writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { InteractiveMode } from "@earendil-works/pi-coding-agent";

import { terminalOnlyClipboard } from "./terminal-copy.ts";

export const KITTY_CLIPBOARD_PREFIX = "\x1b]5522;";
export const KITTY_READ_REQUEST = `${KITTY_CLIPBOARD_PREFIX}type=read;${Buffer.from("image/png").toString("base64")}\x1b\\`;
export const KITTY_ANSWER_TIMEOUT_MS = 30_000;
export const NO_ROUTE_NOTE =
	"Ctrl+V can't reach your laptop clipboard over SSH in this terminal · scp the image and paste its path, or run cloudthinker remote <host>";
export const TMUX_NOTE =
	"Ctrl+V can't reach your laptop clipboard through tmux · scp the image and paste its path, or run cloudthinker remote <host>";
export const WAITING_NOTE = "Asking your terminal for the clipboard image · allow it in the terminal's prompt";
export const EMPTY_NOTE = "Your clipboard holds no image";
export const REFUSED_NOTE = "Your terminal refused the clipboard read · allow it in kitty's prompt or set clipboard_control to read-clipboard";

export type PasteEnv = Readonly<Record<string, string | undefined>>;
export type PasteRoute = "local" | "kitty" | "tmux" | "none";

export type ClipboardImage =
	| { kind: "image"; bytes: Buffer }
	| { kind: "empty" }
	| { kind: "refused" }
	| { kind: "unsupported" };

export interface KittyReply {
	status: string;
	mime?: string;
	payload: string;
}

export interface PasteInput {
	consume?: boolean;
}

export interface PasteHost {
	ui: {
		addInputListener(listener: (data: string) => PasteInput | undefined): () => void;
		requestRender(): void;
	};
	editor: { insertTextAtCursor?: (text: string) => void };
	showStatus(message: string): void;
	showError(message: string): void;
	handleClipboardPaste?: () => Promise<void>;
}

export interface PastePrototype {
	handleClipboardPaste?: (this: PasteHost) => Promise<void>;
}

export function pasteRoute(env: PasteEnv = process.env, platform: NodeJS.Platform = process.platform): PasteRoute {
	if (env.CLOUDTHINKER_REMOTE === "1" || !terminalOnlyClipboard(env, platform)) return "local";
	if (env.TMUX) return "tmux";
	return env.TERM?.includes("kitty") ? "kitty" : "none";
}

export function parseKittyReply(sequence: string): KittyReply | undefined {
	if (!sequence.startsWith(KITTY_CLIPBOARD_PREFIX)) return undefined;
	const body = sequence.slice(KITTY_CLIPBOARD_PREFIX.length).replace(/(?:\x1b\\|\x07)$/, "");
	const split = body.indexOf(";");
	const meta = split === -1 ? body : body.slice(0, split);
	const payload = split === -1 ? "" : body.slice(split + 1);
	const fields = new Map(meta.split(":").map((field) => field.split("=", 2) as [string, string | undefined]));
	if (fields.get("type") !== "read") return undefined;
	const mime = fields.get("mime");
	return {
		status: fields.get("status") ?? "",
		payload,
		...(mime ? { mime: Buffer.from(mime, "base64").toString() } : {}),
	};
}

export function readKittyImage(
	ui: PasteHost["ui"],
	write: (data: string) => void = (data) => void process.stdout.write(data),
	timeoutMs: number = KITTY_ANSWER_TIMEOUT_MS,
): Promise<ClipboardImage> {
	return new Promise((resolve) => {
		const chunks: string[] = [];
		const finish = (result: ClipboardImage) => {
			clearTimeout(timer);
			remove();
			resolve(result);
		};
		const timer = setTimeout(() => finish({ kind: "unsupported" }), timeoutMs);
		const remove = ui.addInputListener((data) => {
			const reply = parseKittyReply(data);
			if (!reply) return undefined;
			if (reply.status === "DATA" && reply.mime === "image/png") chunks.push(reply.payload);
			else if (reply.status === "DONE") {
				const bytes = Buffer.from(chunks.join(""), "base64");
				finish(bytes.length > 0 ? { kind: "image", bytes } : { kind: "empty" });
			} else if (reply.status === "EPERM") finish({ kind: "refused" });
			else if (reply.status !== "OK" && reply.status !== "DATA") finish({ kind: "unsupported" });
			return { consume: true };
		});
		write(KITTY_READ_REQUEST);
	});
}

export function saveClipboardImage(bytes: Buffer, directory: string = tmpdir()): string {
	const path = join(directory, `pi-clipboard-${randomUUID()}.png`);
	writeFileSync(path, bytes);
	return path;
}

async function pasteFromKitty(host: PasteHost): Promise<void> {
	host.showStatus(WAITING_NOTE);
	const result = await readKittyImage(host.ui);
	if (result.kind === "image") {
		host.editor.insertTextAtCursor?.(saveClipboardImage(result.bytes));
		host.ui.requestRender();
		return;
	}
	host.showStatus(result.kind === "empty" ? EMPTY_NOTE : result.kind === "refused" ? REFUSED_NOTE : NO_ROUTE_NOTE);
}

export function wrapSshPaste(prototype: PastePrototype, env: PasteEnv = process.env): void {
	const handleClipboardPaste = prototype.handleClipboardPaste;
	if (typeof handleClipboardPaste !== "function") {
		throw new Error("pi's InteractiveMode no longer defines handleClipboardPaste, so Ctrl+V cannot paste an image over SSH");
	}
	prototype.handleClipboardPaste = async function () {
		const route = pasteRoute(env);
		if (route === "local") return handleClipboardPaste.call(this);
		if (route === "kitty") return pasteFromKitty(this);
		this.showStatus(route === "tmux" ? TMUX_NOTE : NO_ROUTE_NOTE);
	};
}

export function applySshPaste(env: PasteEnv = process.env): void {
	wrapSshPaste(InteractiveMode.prototype as unknown as PastePrototype, env);
}
