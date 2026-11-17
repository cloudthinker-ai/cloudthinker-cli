import type { ExtensionUIContext, Theme } from "@earendil-works/pi-coding-agent";
import { sanitizeTerminalText } from "@cloudthinker/cloud/src/awareness.ts";
import { tickMs } from "@cloudthinker/cloud/src/motion.ts";
import { isKeyRelease, Key, matchesKey, truncateToWidth, visibleWidth, wrapTextWithAnsi, type Component, type TUI, type TuiMouseEvent, type TuiMouseEventResult } from "@earendil-works/pi-tui";
import { handleSearchKey, searchPrompt, searchStatus, stepMatch } from "./search-view.ts";
import { plural } from "./terminal-copy.ts";

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
const VIEW_HEIGHT_PERCENT = 80;
const TICK_MS = tickMs();
export const SPINNER = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

export function spinnerFrame(now = Date.now()): string {
	return SPINNER[Math.floor(now / TICK_MS) % SPINNER.length]!;
}

export function formatClock(ms: number): string {
	const seconds = Math.max(0, Math.floor(ms / 1000));
	if (seconds < 60) return `${seconds}s`;
	const minutes = Math.floor(seconds / 60);
	if (minutes < 60) return `${minutes}m ${seconds % 60}s`;
	return `${Math.floor(minutes / 60)}h ${minutes % 60}m`;
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
			overlayOptions: { anchor: "center", width: "90%", maxHeight: `${VIEW_HEIGHT_PERCENT}%` },
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
		this.ticker ??= setInterval(() => this.tui?.requestRender(), TICK_MS).unref();
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
	private text: string | undefined;
	private width = 0;
	private lines: string[] = [];
	private rows: string[] = [];
	private rowLine: number[] = [];
	private top = 0;
	private follow = true;
	private searching = false;
	private query = "";
	private matches: number[] = [];
	private current = -1;

	constructor(tui: TUI, theme: Theme, task: CommandTask, done: () => void) {
		this.tui = tui;
		this.theme = theme;
		this.task = task;
		this.done = done;
		this.timer = setInterval(() => this.tui.requestRender(), Math.max(250, TICK_MS));
		this.timer.unref();
	}

	private height(): number {
		return Math.max(4, Math.floor((this.tui.terminal.rows * VIEW_HEIGHT_PERCENT) / 100));
	}

	private roomy(): boolean {
		return this.height() >= 12;
	}

	private bodyHeight(): number {
		return Math.max(1, this.height() - (this.roomy() ? 6 : 4));
	}

	private maxTop(): number {
		return Math.max(0, this.rows.length - this.bodyHeight());
	}

	private refresh(inner: number): void {
		const text = this.task.output();
		if (text === this.text && inner === this.width) return;
		this.text = text;
		this.width = inner;
		const lines = text.replace(/\r\n/g, "\n").trimEnd().split("\n").map((line) => sanitizeTerminalText(line.split("\r").at(-1)!));
		this.lines = lines.length === 1 && lines[0] === "" ? [] : lines;
		this.rows = [];
		this.rowLine = [];
		this.lines.forEach((line, index) => {
			for (const row of wrapTextWithAnsi(line, inner)) {
				this.rows.push(row);
				this.rowLine.push(index);
			}
		});
		if (this.query) this.computeMatches();
	}

	private scrollTo(top: number): void {
		this.top = Math.min(this.maxTop(), Math.max(0, top));
		this.follow = this.top >= this.maxTop();
	}

	private computeMatches(): void {
		const needle = this.query.toLowerCase();
		this.matches = needle ? this.lines.flatMap((line, index) => (line.toLowerCase().includes(needle) ? [index] : [])) : [];
		if (this.current >= this.matches.length) this.current = this.matches.length - 1;
	}

	private jump(direction: 1 | -1): void {
		if (this.matches.length === 0) return;
		if (this.current === -1) {
			const topLine = this.rowLine[this.top] ?? 0;
			const index = direction === 1 ? this.matches.findIndex((line) => line >= topLine) : this.matches.findLastIndex((line) => line <= topLine);
			this.current = index === -1 ? (direction === 1 ? 0 : this.matches.length - 1) : index;
		} else {
			this.current = stepMatch(this.current, this.matches.length, direction);
		}
		const row = this.rowLine.indexOf(this.matches[this.current]!);
		const body = this.bodyHeight();
		if (row < this.top || row >= this.top + body) this.scrollTo(row - Math.floor(body / 3));
		else this.follow = false;
	}

	private highlight(row: string, current: boolean): string {
		const theme = this.theme;
		const needle = this.query.toLowerCase();
		const lower = row.toLowerCase();
		let out = "";
		let at = 0;
		for (let hit = lower.indexOf(needle); needle && hit !== -1; hit = lower.indexOf(needle, at)) {
			out += theme.fg("toolOutput", row.slice(at, hit));
			const found = row.slice(hit, hit + needle.length);
			out += current ? theme.bg("selectedBg", theme.fg("accent", found)) : theme.fg("warning", found);
			at = hit + needle.length;
		}
		return out + theme.fg("toolOutput", row.slice(at));
	}

	render(width: number): string[] {
		const theme = this.theme;
		const inner = Math.max(10, width - 4);
		this.refresh(inner);
		if (this.follow) this.top = this.maxTop();
		this.top = Math.min(this.top, this.maxTop());
		const body = this.bodyHeight();
		const matched = new Set(this.matches);
		const currentLine = this.current >= 0 ? this.matches[this.current] : undefined;
		const visible: string[] = [];
		for (let index = this.top; index < this.top + body; index += 1) {
			const row = this.rows[index];
			if (row === undefined) visible.push("");
			else if (matched.has(this.rowLine[index]!)) visible.push(this.highlight(row, this.rowLine[index] === currentLine));
			else visible.push(theme.fg("toolOutput", row));
		}
		if (this.rows.length === 0) visible[0] = theme.fg("muted", "waiting for output…");
		const running = this.task.running();
		const state = this.stopping ? "stopping…" : running ? `running · ${formatClock(Date.now() - this.task.startedAt)}` : "finished";
		const row = (text: string) => {
			const clipped = truncateToWidth(text, inner);
			return `${theme.fg("dim", "│")} ${clipped}${" ".repeat(Math.max(0, inner - visibleWidth(clipped)))} ${theme.fg("dim", "│")}`;
		};
		const gap = this.roomy() ? [row("")] : [];
		return [
			theme.fg("dim", `╭${"─".repeat(inner + 2)}╮`),
			row(`${theme.fg("toolTitle", theme.bold(`$ ${this.task.command}`))} ${theme.fg("muted", `· ${state}`)}`),
			...gap,
			...visible.map(row),
			...gap,
			row(this.footer(running, inner)),
			theme.fg("dim", `╰${"─".repeat(inner + 2)}╯`),
		];
	}

	private footer(running: boolean, inner: number): string {
		const theme = this.theme;
		if (this.searching) return searchPrompt(theme, this.query, "");
		const found = this.query
			? searchStatus(theme, this.query, this.matches.length, this.current, "n/N · ")
			: "";
		const total = this.lines.length;
		const first = (this.rowLine[this.top] ?? 0) + 1;
		const last = (this.rowLine[Math.min(this.rows.length, this.top + this.bodyHeight()) - 1] ?? 0) + 1;
		const position = total > 0 ? `line ${first === last ? first : `${first}–${last}`} of ${total} · ` : "";
		const follows = this.follow ? "" : "end follows · ";
		const stop = running ? "x stop · " : "";
		const options = [
			`${found}${theme.fg("dim", `${position}${follows}↑↓ scroll · / search · ${stop}esc close`)}`,
			`${found}${theme.fg("dim", `${position}${follows}${stop}esc close`)}`,
			`${found}${theme.fg("dim", `${position}${stop}esc close`)}`,
			`${found}${theme.fg("dim", `${stop}esc close`)}`,
		];
		return options.find((option) => visibleWidth(option) <= inner) ?? options.at(-1)!;
	}

	handleInput(data: string): void {
		if (isKeyRelease(data)) return;
		if (this.searching) {
			this.handleSearchInput(data);
			this.tui.requestRender();
			return;
		}
		const page = Math.max(1, this.bodyHeight() - 1);
		if (matchesKey(data, Key.escape) || data === "q") return this.close();
		if (data === "x") {
			if (!this.stopping && this.task.running()) {
				this.stopping = true;
				void this.task.stop().finally(() => this.close());
			}
			return;
		}
		if (data === "j" || matchesKey(data, Key.down)) this.scrollTo(this.top + 1);
		else if (data === "k" || matchesKey(data, Key.up)) this.scrollTo(this.top - 1);
		else if (matchesKey(data, Key.pageDown)) this.scrollTo(this.top + page);
		else if (matchesKey(data, Key.pageUp)) this.scrollTo(this.top - page);
		else if (data === "g" || matchesKey(data, Key.home)) this.scrollTo(0);
		else if (data === "G" || matchesKey(data, Key.end)) this.scrollTo(this.maxTop());
		else if (data === "/") {
			this.searching = true;
			this.query = "";
			this.matches = [];
			this.current = -1;
		} else if (data === "n") this.jump(1);
		else if (data === "N") this.jump(-1);
		else return;
		this.tui.requestRender();
	}

	private handleSearchInput(data: string): void {
		const key = handleSearchKey(this.query, data);
		if (!key) return;
		if (key.kind === "cancel") {
			this.searching = false;
			this.query = "";
			this.matches = [];
			this.current = -1;
		} else if (key.kind === "confirm") {
			this.searching = false;
			this.computeMatches();
			this.current = -1;
			this.jump(1);
		} else this.query = key.query;
	}

	handleMouse(event: TuiMouseEvent): TuiMouseEventResult | undefined {
		if (event.type !== "wheel" || event.wheelDelta === undefined) return { handled: true, render: false };
		this.scrollTo(this.top + event.wheelDelta);
		return { handled: true };
	}

	invalidate(): void {
		this.text = undefined;
	}

	private close(): void {
		clearInterval(this.timer);
		this.done();
	}
}

export const tasksPane = new TasksPane();
