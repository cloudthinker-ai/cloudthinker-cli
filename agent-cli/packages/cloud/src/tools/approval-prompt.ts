import type { Theme } from "@earendil-works/pi-coding-agent";
import { type Component, matchesKey, truncateToWidth, wrapTextWithAnsi } from "@earendil-works/pi-tui";

import { sanitizeTerminalText } from "../awareness.ts";

export const MAX_SCRIPT_LINES = 12;

export interface ApprovalChoice {
	label: string;
	key: string;
}

export interface ApprovalRequest {
	title: string;
	reasoning: string;
	connections: string;
	script: string;
	webUrl: string;
	choices: readonly ApprovalChoice[];
}

function clean(text: string): string[] {
	return text.split("\n").map((line) => sanitizeTerminalText(line).trimEnd());
}

export class ApprovalPrompt implements Component {
	private readonly request: ApprovalRequest;
	private readonly theme: Theme;
	private readonly done: (choice: string | undefined) => void;
	private readonly requestRender: () => void;
	private selected = 0;

	constructor(request: ApprovalRequest, theme: Theme, done: (choice: string | undefined) => void, requestRender: () => void) {
		this.request = request;
		this.theme = theme;
		this.done = done;
		this.requestRender = requestRender;
	}

	render(width: number): string[] {
		const theme = this.theme;
		const inner = Math.max(10, width - 4);
		const wrap = (text: string, color: "text" | "muted" | "dim"): string[] =>
			clean(text).flatMap((line) => wrapTextWithAnsi(line, inner)).map((line) => `   ${theme.fg(color, line)}`);
		const script = clean(this.request.script);
		const shown = script.slice(0, MAX_SCRIPT_LINES);
		const lines = [
			theme.fg("border", "─".repeat(width)),
			` ${theme.bold(theme.fg("warning", "approval needed:"))} ${theme.bold(theme.fg("warning", this.request.title))}`,
			...wrap(this.request.reasoning, "text"),
			`   ${theme.fg("muted", `cloud · ${sanitizeTerminalText(this.request.connections)}`)}`,
			...shown.flatMap((line) => wrapTextWithAnsi(line, inner - 2)).map((line) => `   ${theme.fg("dim", "▎")} ${theme.fg("text", line)}`),
		];
		if (script.length > shown.length) {
			lines.push(`   ${theme.fg("dim", `▎ … ${script.length - shown.length} more lines`)}`);
		}
		lines.push(`   ${theme.fg("dim", `browser → ${sanitizeTerminalText(this.request.webUrl)}`)}`, "");
		this.request.choices.forEach((choice, index) => {
			const active = index === this.selected;
			const label = `${index + 1}. ${choice.label}`;
			const row = active ? `${theme.fg("accent", "›")} ${theme.bold(theme.fg("accent", label))}` : `  ${theme.fg("text", label)}`;
			lines.push(` ${row}  ${theme.fg("dim", choice.key)}`);
		});
		const keys = this.request.choices.map((choice) => choice.key).join("/");
		lines.push("", ` ${theme.fg("dim", `↑↓ choose · enter confirm · ${keys} pick · esc decline`)}`, theme.fg("border", "─".repeat(width)));
		return lines.map((line) => truncateToWidth(line, width));
	}

	handleInput(data: string): void {
		const choices = this.request.choices;
		if (matchesKey(data, "escape") || matchesKey(data, "ctrl+c")) {
			this.done(undefined);
			return;
		}
		if (matchesKey(data, "enter") || matchesKey(data, "return")) {
			this.done(choices[this.selected]?.label);
			return;
		}
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

	invalidate(): void {}
}
