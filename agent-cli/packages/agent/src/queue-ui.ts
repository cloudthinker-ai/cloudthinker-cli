import { InteractiveMode } from "@earendil-works/pi-coding-agent";
import { type Container, Spacer, TruncatedText } from "@earendil-works/pi-tui";

import { theme } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/theme/theme.js";
import { completionRows } from "./background/index.ts";

export const QUEUED_TITLE = "Queued";
export const STEERING_NOTE = "after this step";
export const FOLLOW_UP_NOTE = "after this turn";

interface QueueStyler {
	fg(color: "dim" | "muted", text: string): string;
	bold(text: string): string;
}

interface PendingHost {
	pendingMessagesContainer: Container;
	getAllQueuedMessages(): { steering: string[]; followUp: string[] };
	getAppKeyDisplay(action: string): string;
}

interface PendingPrototype {
	updatePendingMessagesDisplay(this: PendingHost): void;
	getAllQueuedMessages?: unknown;
	getAppKeyDisplay?: unknown;
}

const plain = { fg: (_color: string, text: string) => text, bold: (text: string) => text } as Parameters<typeof completionRows>[1];

function queuedLine(message: string, note: string, styler: QueueStyler): string {
	const rows = completionRows(message, plain);
	const lines = rows ? rows.map((row) => row.trim()) : message.trim().split("\n");
	const more = lines.length > 1 ? ` (+${lines.length - 1} ${rows ? "more" : "lines"})` : "";
	return `${styler.fg("dim", "  › ")}${styler.fg("muted", lines[0] ?? "")}${styler.fg("dim", `${more} · ${note}`)}`;
}

export function queuedLines(
	steering: readonly string[],
	followUp: readonly string[],
	dequeueKey: string,
	styler: QueueStyler,
): string[] {
	if (steering.length === 0 && followUp.length === 0) return [];
	const count = steering.length + followUp.length;
	const edit = dequeueKey ? ` · ${dequeueKey} to edit` : "";
	return [
		`${styler.fg("muted", styler.bold(`${QUEUED_TITLE} ${count}`))}${styler.fg("dim", edit)}`,
		...steering.map((message) => queuedLine(message, STEERING_NOTE, styler)),
		...followUp.map((message) => queuedLine(message, FOLLOW_UP_NOTE, styler)),
	];
}

export function applyQueuedMessagesUi(): void {
	const prototype = InteractiveMode.prototype as unknown as PendingPrototype;
	if (
		typeof prototype.updatePendingMessagesDisplay !== "function" ||
		typeof prototype.getAllQueuedMessages !== "function" ||
		typeof prototype.getAppKeyDisplay !== "function"
	) {
		throw new Error("pi's queued message display seam changed");
	}
	prototype.updatePendingMessagesDisplay = function (this: PendingHost) {
		this.pendingMessagesContainer.clear();
		const { steering, followUp } = this.getAllQueuedMessages();
		const lines = queuedLines(steering, followUp, this.getAppKeyDisplay("app.message.dequeue"), theme);
		if (lines.length === 0) return;
		this.pendingMessagesContainer.addChild(new Spacer(1));
		for (const line of lines) this.pendingMessagesContainer.addChild(new TruncatedText(line, 1, 0));
	};
}
