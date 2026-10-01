import type { ExtensionUIContext, Theme } from "@earendil-works/pi-coding-agent";
import { truncateToWidth, type TUI } from "@earendil-works/pi-tui";

export type TaskRows = (tui: TUI, theme: Theme) => string[];

interface TaskGroup {
	label: string;
	active: number;
	rows: TaskRows;
}

const WIDGET_KEY = "ct-tasks";
const GROUP_ORDER = ["Agents", "Commands"];
export const SPINNER = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

export function spinnerFrame(now = Date.now()): string {
	return SPINNER[Math.floor(now / 80) % SPINNER.length]!;
}

export class TasksPane {
	private ui: ExtensionUIContext | undefined;
	private tui: TUI | undefined;
	private registered = false;
	private readonly groups = new Map<string, TaskGroup>();

	bind(ui: ExtensionUIContext): void {
		if (ui === this.ui) return;
		this.ui = ui;
		this.registered = false;
		this.tui = undefined;
		this.update();
	}

	setGroup(label: string, rows: TaskRows | undefined, active = 0): void {
		if (rows) this.groups.set(label, { label, active, rows });
		else this.groups.delete(label);
		this.update();
	}

	render(tui: TUI, theme: Theme): string[] {
		const width = tui.terminal.columns;
		const groups = [...this.groups.values()].sort((a, b) => GROUP_ORDER.indexOf(a.label) - GROUP_ORDER.indexOf(b.label));
		const active = groups.some((group) => group.active > 0);
		const color = active ? "accent" : "dim";
		const lines = [`${theme.fg(color, active ? "●" : "○")} ${theme.fg(color, theme.bold("Tasks"))}`];
		for (const group of groups) {
			const rows = group.rows(tui, theme);
			if (rows.length === 0) continue;
			const count = group.active > 0 ? ` ${theme.fg("muted", String(group.active))}` : "";
			lines.push(`${theme.fg("dim", "▾")} ${theme.fg("text", group.label)}${count}`, ...rows);
		}
		return lines.map((line) => truncateToWidth(line, width));
	}

	private update(): void {
		const ui = this.ui;
		if (!ui) return;
		if (this.groups.size === 0) {
			if (this.registered) ui.setWidget(WIDGET_KEY, undefined);
			this.registered = false;
			this.tui = undefined;
			return;
		}
		if (this.registered) {
			this.tui?.requestRender();
			return;
		}
		ui.setWidget(WIDGET_KEY, (tui, theme) => {
			this.tui = tui;
			return {
				render: () => this.render(tui, theme),
				invalidate: () => {
					this.registered = false;
					this.tui = undefined;
				},
			};
		}, { placement: "aboveEditor" });
		this.registered = true;
	}
}

export const tasksPane = new TasksPane();
