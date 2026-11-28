import { createInterface } from "node:readline";

import type { Theme } from "@earendil-works/pi-coding-agent";
import { type Component, matchesKey, type TuiMouseEvent, type TuiMouseEventResult, truncateToWidth, wrapTextWithAnsi } from "@earendil-works/pi-tui";

import { sanitizeTerminalText } from "../awareness.ts";

export const CHROME_ROWS = 3;
const MIN_DETAIL_ROWS = 4;

export interface ApprovalChoice {
	label: string;
	key: string;
}

export interface ApprovalRequest {
	title: string;
	reasoning: string;
	place?: string;
	connections: string;
	script: string;
	webUrl: string;
	choices: readonly ApprovalChoice[];
}

function clean(text: string): string[] {
	return text.split("\n").map((line) => sanitizeTerminalText(line).trimEnd());
}

export function hasPrintApprovalTerminal(mode: string | undefined): boolean {
	return mode === "print" &&
		process.stdin.isTTY === true &&
		process.stderr.isTTY === true;
}

let printApprovalQueue = Promise.resolve();

export function askPrintApproval(
	request: Omit<ApprovalRequest, "choices">,
	signal?: AbortSignal,
): Promise<"approve" | "decline" | "browser"> {
	const decision = printApprovalQueue.then(async (): Promise<"approve" | "decline" | "browser"> => {
		signal?.throwIfAborted();
		const details = [
			request.title,
			...clean(request.reasoning),
			...clean(request.connections),
			...clean(request.script).map((line) => `  ${line}`),
			...(request.webUrl ? [`Browser: ${sanitizeTerminalText(request.webUrl)}`] : []),
			"Approve [y], decline [n], or press Enter to decide in your browser: ",
		];
		process.stderr.write(details.join("\n"));
		const line = await new Promise<string | undefined>((resolve) => {
			const input = createInterface({ input: process.stdin, terminal: false });
			const abort = () => input.close();
			input.once("close", () => {
				signal?.removeEventListener("abort", abort);
				resolve(undefined);
			});
			input.once("line", (answer) => {
				resolve(answer);
				input.close();
			});
			signal?.addEventListener("abort", abort, { once: true });
			if (signal?.aborted) abort();
		});
		signal?.throwIfAborted();
		const answer = line?.trim().toLowerCase();
		if (answer === "y" || answer === "yes") return "approve";
		if (answer === "n" || answer === "no") return "decline";
		return "browser";
	});
	printApprovalQueue = decision.then(() => {}, () => {});
	return decision;
}

interface Layout {
	merged: boolean;
	detail: number;
	browser: boolean;
	borderTop: boolean;
	borderBottom: boolean;
	gapChoices: boolean;
	gapKeys: boolean;
}

export class ApprovalPrompt implements Component {
	private readonly request: ApprovalRequest;
	private readonly theme: Theme;
	private readonly done: (choice: string | undefined) => void;
	private readonly requestRender: () => void;
	private readonly rows: () => number;
	private selected = 0;
	private top = 0;
	private view = 0;
	private total = 0;
	private seenEnd = false;

	constructor(
		request: ApprovalRequest,
		theme: Theme,
		done: (choice: string | undefined) => void,
		requestRender: () => void,
		rows: () => number,
	) {
		this.request = request;
		this.theme = theme;
		this.done = done;
		this.requestRender = requestRender;
		this.rows = rows;
	}

	private details(inner: number): string[] {
		const theme = this.theme;
		return [
			...clean(this.request.reasoning).flatMap((line) => wrapTextWithAnsi(line, inner)).map((line) => `   ${theme.fg("text", line)}`),
			`   ${theme.fg("muted", `${this.request.place ?? "cloud"} · ${sanitizeTerminalText(this.request.connections)}`)}`,
			...clean(this.request.script).flatMap((line) => wrapTextWithAnsi(line, inner - 2)).map((line) => `   ${theme.fg("dim", "▎")} ${theme.fg("text", line)}`),
		];
	}

	private layout(budget: number, total: number): Layout {
		const choices = this.request.choices.length;
		let spare = budget - choices - 2;
		const merged = spare < 0;
		if (merged) spare += 1;
		const layout: Layout = { merged, detail: 0, browser: false, borderTop: false, borderBottom: false, gapChoices: false, gapKeys: false };
		const extras: (keyof Omit<Layout, "merged" | "detail">)[] = [
			...(this.request.webUrl ? ["browser" as const] : []),
			"gapChoices",
			"borderTop",
			"borderBottom",
			"gapKeys",
		];
		if (total <= spare) {
			layout.detail = total;
			spare -= total;
			for (const extra of extras) {
				if (spare <= 0) break;
				layout[extra] = true;
				spare -= 1;
			}
			return layout;
		}
		if (spare < 2) return layout;
		spare -= 1;
		for (const extra of extras) {
			if (spare - 1 < MIN_DETAIL_ROWS) break;
			layout[extra] = true;
			spare -= 1;
		}
		layout.detail = Math.max(0, spare);
		return layout;
	}

	private scrollTo(top: number): void {
		this.top = Math.min(Math.max(0, this.total - this.view), Math.max(0, top));
		if (this.top + this.view >= this.total) this.seenEnd = true;
		this.requestRender();
	}

	render(width: number): string[] {
		const theme = this.theme;
		const inner = Math.max(10, width - 4);
		const details = this.details(inner);
		const layout = this.layout(Math.max(1, this.rows() - CHROME_ROWS), details.length);
		this.total = details.length;
		this.view = layout.detail;
		this.top = Math.min(this.top, Math.max(0, this.total - this.view));
		if (this.top + this.view >= this.total) this.seenEnd = true;
		const overflow = this.total > this.view;
		const guarded = overflow && this.view > 0 && !this.seenEnd;
		const keys = this.request.choices.map((choice) => choice.key).join("/");
		const scroll = overflow && this.view > 0 ? "pgdn scroll · " : "";
		const enter = guarded ? "enter reads to end" : "enter confirm";
		const hints = ` ${theme.fg("dim", `↑↓ choose · ${scroll}${enter} · ${keys} · esc decline`)}`;
		const title = ` ${theme.bold(theme.fg("warning", "approval needed:"))} ${theme.bold(theme.fg("warning", this.request.title))}`;
		const lines: string[] = [];
		if (layout.borderTop) lines.push(theme.fg("border", "─".repeat(width)));
		const hidden = this.view === 0 ? "script hidden · " : "";
		lines.push(layout.merged ? ` ${theme.bold(theme.fg("warning", "approval needed"))} ${theme.fg("dim", `${hidden}${keys} · esc decline`)}` : title);
		lines.push(...details.slice(this.top, this.top + this.view));
		if (overflow && this.view > 0) {
			const more = this.top + this.view < this.total ? " · pgdn more" : "";
			lines.push(`   ${theme.fg("dim", `▎ lines ${this.top + 1}–${this.top + this.view} of ${this.total}${more}`)}`);
		} else if (overflow && !layout.merged) {
			lines.push(`   ${theme.fg("dim", "▎ script hidden: the terminal is too short")}`);
		}
		if (layout.browser) lines.push(`   ${theme.fg("dim", `browser → ${sanitizeTerminalText(this.request.webUrl)}`)}`);
		if (layout.gapChoices) lines.push("");
		this.request.choices.forEach((choice, index) => {
			const active = index === this.selected;
			const label = `${index + 1}. ${choice.label}`;
			const row = active ? `${theme.fg("accent", "›")} ${theme.bold(theme.fg("accent", label))}` : `  ${theme.fg("text", label)}`;
			lines.push(` ${row}  ${theme.fg("dim", choice.key)}`);
		});
		if (!layout.merged) {
			if (layout.gapKeys) lines.push("");
			lines.push(hints);
		}
		if (layout.borderBottom) lines.push(theme.fg("border", "─".repeat(width)));
		return lines.map((line) => truncateToWidth(line, width));
	}

	handleInput(data: string): void {
		const choices = this.request.choices;
		if (matchesKey(data, "escape") || matchesKey(data, "ctrl+c")) {
			this.done(undefined);
			return;
		}
		if (matchesKey(data, "enter") || matchesKey(data, "return")) {
			if (!this.seenEnd && this.total > this.view && this.view > 0) {
				this.scrollTo(this.total - this.view);
				return;
			}
			this.done(choices[this.selected]?.label);
			return;
		}
		if (matchesKey(data, "pageDown")) return this.scrollTo(this.top + Math.max(1, this.view - 1));
		if (matchesKey(data, "pageUp")) return this.scrollTo(this.top - Math.max(1, this.view - 1));
		if (matchesKey(data, "home")) return this.scrollTo(0);
		if (matchesKey(data, "end")) return this.scrollTo(this.total);
		if (matchesKey(data, "up") || data === "k") {
			this.selected = (this.selected - 1 + choices.length) % choices.length;
			this.requestRender();
			return;
		}
		if (matchesKey(data, "down") || data === "j" || matchesKey(data, "tab")) {
			this.selected = (this.selected + 1) % choices.length;
			this.requestRender();
			return;
		}
		const index = /^[1-9]$/.test(data) ? Number(data) - 1 : choices.findIndex((choice) => choice.key === data.toLowerCase());
		const choice = choices[index];
		if (choice) this.done(choice.label);
	}

	handleMouse(event: TuiMouseEvent): TuiMouseEventResult | undefined {
		if (event.type !== "wheel" || event.wheelDelta === undefined) return undefined;
		this.scrollTo(this.top + event.wheelDelta);
		return { handled: true };
	}

	invalidate(): void {}
}
