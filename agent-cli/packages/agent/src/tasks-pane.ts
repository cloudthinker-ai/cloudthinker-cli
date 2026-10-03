import type { ExtensionUIContext, Theme } from "@earendil-works/pi-coding-agent";
import { sanitizeTerminalText } from "@cloudthinker/cloud/src/awareness.ts";
import { isKeyRelease, Key, matchesKey, truncateToWidth, visibleWidth, type Component, type TUI } from "@earendil-works/pi-tui";

export interface CommandTask {
	id: string;
	command: string;
	startedAt: number;
	tail: () => string;
	output: () => string;
	running: () => boolean;
	stop: () => Promise<void>;
}

const WIDGET_KEY = "ct-tasks";
const MANAGE_HINT = "↓ to manage";
const VIEW_LINES = 20;
export const SPINNER = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

export function spinnerFrame(now = Date.now()): string {
	return SPINNER[Math.floor(now / 80) % SPINNER.length]!;
}

export function formatClock(ms: number): string {
	const seconds = Math.max(0, Math.floor(ms / 1000));
	if (seconds < 60) return `${seconds}s`;
	const minutes = Math.floor(seconds / 60);
	if (minutes < 60) return `${minutes}m ${seconds % 60}s`;
	return `${Math.floor(minutes / 60)}h ${minutes % 60}m`;
}

function plural(count: number, noun: string): string {
	return `${count} ${noun}${count === 1 ? "" : "s"}`;
}

export class TasksPane {
	private ui: ExtensionUIContext | undefined;
	private tui: TUI | undefined;
	private registered = false;
	private ticker: NodeJS.Timeout | undefined;
	private agents = 0;
	private commandTasks: CommandTask[] = [];
	managed = false;

	bind(ui: ExtensionUIContext): void {
		if (ui === this.ui) return;
		this.ui = ui;
		this.registered = false;
		this.tui = undefined;
		this.update();
	}

	setGroup(label: string, _rows: unknown, active = 0): void {
		if (label === "Agents") this.agents = active;
		this.update();
	}

	setCommands(tasks: CommandTask[]): void {
		this.commandTasks = tasks;
		this.update();
	}

	get commands(): readonly CommandTask[] {
		return this.commandTasks;
	}

	summary(now = Date.now()): string | undefined {
		const commands = this.commandTasks;
		if (this.agents === 0 && commands.length === 0) return undefined;
		if (this.agents === 0 && commands.length === 1) {
			const only = commands[0]!;
			const tail = only.tail();
			return `$ ${only.command} · ${formatClock(now - only.startedAt)}${tail ? ` · ${tail}` : ""}`;
		}
		return [this.agents > 0 ? plural(this.agents, "agent") : "", commands.length > 0 ? plural(commands.length, "command") : ""]
			.filter(Boolean)
			.join(" · ");
	}

	render(tui: TUI, theme: Theme): string[] {
		const summary = this.summary();
		if (!summary) return [];
		const hint = this.managed ? `  ${theme.fg("dim", MANAGE_HINT)}` : "";
		return [truncateToWidth(`${theme.fg("accent", spinnerFrame())} ${theme.fg("muted", summary)}${hint}`, tui.terminal.columns)];
	}

	open(id: string, ui: ExtensionUIContext): Promise<void> {
		const task = this.commandTasks.find((item) => item.id === id);
		if (!task) return Promise.resolve();
		return ui.custom<void>((tui, theme, _keybindings, done) => new CommandView(tui, theme, task, done), {
			overlay: true,
			overlayOptions: { anchor: "center", width: "90%", maxHeight: "80%" },
		});
	}

	private update(): void {
		const ui = this.ui;
		if (!ui) return;
		const active = this.agents > 0 || this.commandTasks.length > 0;
		if (!active) {
			if (this.registered) ui.setWidget(WIDGET_KEY, undefined);
			this.registered = false;
			this.tui = undefined;
			if (this.ticker) clearInterval(this.ticker);
			this.ticker = undefined;
			return;
		}
		this.ticker ??= setInterval(() => this.tui?.requestRender(), 80).unref();
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

class CommandView implements Component {
	private readonly timer: NodeJS.Timeout;
	private stopping = false;
	private readonly tui: TUI;
	private readonly theme: Theme;
	private readonly task: CommandTask;
	private readonly done: () => void;

	constructor(tui: TUI, theme: Theme, task: CommandTask, done: () => void) {
		this.tui = tui;
		this.theme = theme;
		this.task = task;
		this.done = done;
		this.timer = setInterval(() => this.tui.requestRender(), 250);
		this.timer.unref();
	}

	render(width: number): string[] {
		const theme = this.theme;
		const inner = Math.max(10, width - 4);
		const lines = this.task.output().replace(/\r\n/g, "\n").trimEnd().split("\n").slice(-VIEW_LINES).map((line) => sanitizeTerminalText(line.split("\r").at(-1)!));
		const body = lines.length === 1 && lines[0] === "" ? [theme.fg("muted", "waiting for output…")] : lines.map((line) => theme.fg("toolOutput", line));
		const running = this.task.running();
		const state = this.stopping ? "stopping…" : running ? `running · ${formatClock(Date.now() - this.task.startedAt)}` : "finished";
		const row = (text: string) => {
			const clipped = truncateToWidth(text, inner);
			return `${theme.fg("dim", "│")} ${clipped}${" ".repeat(Math.max(0, inner - visibleWidth(clipped)))} ${theme.fg("dim", "│")}`;
		};
		return [
			theme.fg("dim", `╭${"─".repeat(inner + 2)}╮`),
			row(`${theme.fg("toolTitle", theme.bold(`$ ${this.task.command}`))} ${theme.fg("muted", `· ${state}`)}`),
			row(""),
			...body.map(row),
			row(""),
			row(theme.fg("dim", running ? "x stop · esc close" : "esc close")),
			theme.fg("dim", `╰${"─".repeat(inner + 2)}╯`),
		];
	}

	handleInput(data: string): void {
		if (isKeyRelease(data)) return;
		if (matchesKey(data, Key.escape) || data === "q") return this.close();
		if (data === "x" && !this.stopping && this.task.running()) {
			this.stopping = true;
			void this.task.stop().finally(() => this.close());
		}
	}

	invalidate(): void {}

	private close(): void {
		clearInterval(this.timer);
		this.done();
	}
}

export const tasksPane = new TasksPane();
