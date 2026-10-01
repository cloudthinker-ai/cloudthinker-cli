import { resolve } from "node:path";
import { stripVTControlCharacters } from "node:util";
import { Type } from "typebox";
import { Text } from "@earendil-works/pi-tui";
import {
	createLocalBashOperations,
	getAgentDir,
	SettingsManager,
	type ExtensionAPI,
	type ExtensionContext,
	type Theme,
} from "@earendil-works/pi-coding-agent";
import {
	BACKGROUND_DEFAULT_TIMEOUT_SECONDS,
	BackgroundCommandManager,
	type BackgroundTaskSummary,
} from "./manager.ts";
import { spinnerFrame, tasksPane } from "../tasks-pane.ts";

const TOOL_NAME = "ct_background";
const MAX_COMMAND_ROWS = 4;
const OUTPUT_PREVIEW_LINES = 5;
const TOOL_PROMPT = "Start and manage local shell commands by task id. Inspect status/output or cancel; terminal results resume automatically. Print and JSON runs drain started commands before exit.";
const backgroundSchema = Type.Object({
	action: Type.Union([Type.Literal("start"), Type.Literal("status"), Type.Literal("output"), Type.Literal("cancel")]),
	command: Type.Optional(Type.String()),
	taskId: Type.Optional(Type.String()),
	cwd: Type.Optional(Type.String()),
	timeoutSeconds: Type.Optional(Type.Number()),
	afterByte: Type.Optional(Type.Number()),
	maxBytes: Type.Optional(Type.Number()),
});

interface BackgroundParams {
	action: "start" | "status" | "output" | "cancel";
	command?: string;
	taskId?: string;
	cwd?: string;
	timeoutSeconds?: number;
	afterByte?: number;
	maxBytes?: number;
}

export function registerBackgroundCommands(pi: ExtensionAPI): void {
	const managers = new Map<string, Promise<BackgroundCommandManager>>();
	const contexts = new Map<string, ExtensionContext>();
	const statusTimers = new Map<string, NodeJS.Timeout>();
	const commands = new Map<string, string>();
	const sessionKey = (ctx: ExtensionContext) => ctx.sessionManager.getSessionId();
	const renderStatus = (ctx: ExtensionContext, manager: BackgroundCommandManager) => {
		const tasks = manager.list();
		for (const task of tasks) commands.set(task.id, task.command);
		if (!ctx.hasUI) return;
		tasksPane.bind(ctx.ui);
		const active = tasks.filter((task) => task.state === "running").length;
		ctx.ui.setStatus("ct-background", active > 0 ? `${active} running command${active === 1 ? "" : "s"}` : undefined);
		tasksPane.setGroup("Commands", active > 0 ? (_tui, theme) => commandRows(manager, theme) : undefined, active);
		if (active > 0 && !statusTimers.has(sessionKey(ctx))) {
			statusTimers.set(sessionKey(ctx), setInterval(() => renderStatus(ctx, manager), 80).unref());
		} else if (active === 0) {
			const timer = statusTimers.get(sessionKey(ctx));
			if (timer) clearInterval(timer);
			statusTimers.delete(sessionKey(ctx));
		}
	};
	const getManager = async (ctx: ExtensionContext): Promise<BackgroundCommandManager> => {
		const key = sessionKey(ctx);
		contexts.set(key, ctx);
		let managerPromise = managers.get(key);
		if (!managerPromise) {
			let manager!: BackgroundCommandManager;
			const settings = SettingsManager.create(ctx.cwd, getAgentDir());
			const bash = createLocalBashOperations({ shellPath: settings.getShellPath(), cleanupOnExit: true });
			const commandPrefix = settings.getShellCommandPrefix();
			manager = new BackgroundCommandManager({
				storageDirectory: BackgroundCommandManager.sessionStorageDirectory(getAgentDir(), key),
				cwd: ctx.cwd,
				operations: {
					exec: (command, cwd, options) => bash.exec(commandPrefix ? `${commandPrefix}\n${command}` : command, cwd, options),
				},
				onChange: () => {
					const current = contexts.get(key);
					if (current) renderStatus(current, manager!);
				},
				onTerminal: () => {
					const current = contexts.get(key);
					if (!current) return;
					const completed = manager!.takeCompletions();
					if (completed.length === 0) return;
					if (current.mode === "tui" || current.mode === "rpc") {
						pi.sendUserMessage(completionText(completed), { deliverAs: "followUp", expandPromptTemplates: false });
					} else {
						pi.sendMessage(completionMessage(completed), { triggerTurn: true, deliverAs: "followUp" });
					}
				},
			});
			managerPromise = manager.initialize().then(() => manager);
			managers.set(key, managerPromise);
		}
		return managerPromise;
	};

	pi.registerTool({
		name: TOOL_NAME,
		label: "Background command",
		description: TOOL_PROMPT,
		parameters: backgroundSchema,
		promptSnippet: "Run a local shell command in the background and manage its task.",
		promptGuidelines: ["Use ct_background when a command can run alongside other work; continue useful work until its completion event."],
		renderShell: "self",
		renderCall(args, theme, _context) {
			const action = args.action ?? "start";
			const subject = action === "start" ? args.command ?? "" : args.taskId ? commands.get(args.taskId) ?? args.taskId : "all commands";
			const verb = action === "start" ? "" : ` ${theme.fg("muted", action)}`;
			return new Text(`${theme.fg("accent", "●")} ${theme.fg("toolTitle", theme.bold("Background"))}${verb} ${theme.fg("muted", oneLine(subject, 100))}`, 0, 0);
		},
		renderResult(result, options, theme, context) {
			return new Text(resultText(result, options.expanded, theme, context?.args?.action, context?.isError === true), 0, 0);
		},
		async execute(_toolCallId, params: BackgroundParams, _signal, _onUpdate, ctx) {
			const manager = await getManager(ctx);
			if (params.action === "start") {
				if (!params.command) throw new Error("command is required for start");
				const cwd = resolve(ctx.cwd, params.cwd ?? ".");
				const task = await manager.start(params.command, cwd, params.timeoutSeconds);
				return toolResult(`Started background command ${task.id}. It will run for at most ${params.timeoutSeconds ?? BACKGROUND_DEFAULT_TIMEOUT_SECONDS} seconds.`, { task });
			}
			if (params.action === "status") {
				const tasks = params.taskId ? manager.get(params.taskId) : manager.list();
				const items = Array.isArray(tasks) ? tasks : [tasks];
				const text = items.length === 0 ? "No background commands." : items.map((task) => formatTask(task)).join("\n");
				return toolResult(text, { tasks });
			}
			if (!params.taskId) throw new Error(`taskId is required for ${params.action}`);
			if (params.action === "output") {
				const output = manager.readOutput(params.taskId, params.afterByte ?? 0, params.maxBytes);
				return toolResult(`Output bytes ${output.startByte}–${output.nextByte} of ${output.totalBytes}${output.truncated ? " (older output was truncated)" : ""}\n${output.text}`, { taskId: params.taskId, ...output });
			}
			const task = await manager.cancel(params.taskId);
			return toolResult(`Background command ${task.id} is ${task.state}.`, { task });
		},
	});

	pi.on("session_start", async (_event, ctx) => {
		const manager = await getManager(ctx);
		renderStatus(ctx, manager);
		const interrupted = manager.list().filter((task) => task.state === "interrupted");
		if (ctx.hasUI && interrupted.length > 0) ctx.ui.notify(`${interrupted.length} background command${interrupted.length === 1 ? " was" : "s were"} interrupted by a previous shutdown.`, "warning");
	});
	pi.on("before_agent_start", async (_event, ctx) => {
		const managerPromise = managers.get(sessionKey(ctx));
		if (!managerPromise) return;
		const notices = (await managerPromise).takeRecoveryNotices();
		if (notices.length === 0) return;
		return {
			message: {
				customType: "ct_background_recovery",
				content: [{ type: "text", text: `Recovered background command state. Commands from the previous process were not resumed; inspect the recorded status and output before deciding what to do.\n${notices.map((notice) => `- ${oneLine(notice, 200)}`).join("\n")}` }],
				display: true,
			},
		};
	});
	pi.on("agent_settled", async (_event, ctx) => {
		if (ctx.mode !== "print" && ctx.mode !== "json") return;
		const managerPromise = managers.get(sessionKey(ctx));
		if (!managerPromise) return;
		const manager = await managerPromise;
		await manager.waitForActive();
		const completed = manager.takeCompletions();
		if (completed.length > 0) pi.sendMessage(completionMessage(completed), { triggerTurn: true, deliverAs: "followUp" });
	});
	pi.on("session_shutdown", async (_event, ctx) => {
		const key = sessionKey(ctx);
		const managerPromise = managers.get(key);
		const timer = statusTimers.get(key);
		if (timer) clearInterval(timer);
		statusTimers.delete(key);
		contexts.delete(key);
		if (managerPromise) await (await managerPromise).shutdown();
		managers.delete(key);
	});

}

function completionMessage(tasks: BackgroundTaskSummary[]) {
	return {
		customType: "ct_background_completion",
		content: [{ type: "text" as const, text: completionText(tasks) }],
		display: true,
	};
}

function commandRows(manager: BackgroundCommandManager, theme: Theme): string[] {
	const running = manager.list().filter((task) => task.state === "running");
	const visible = running.slice(-MAX_COMMAND_ROWS);
	const hidden = running.length - visible.length;
	const now = Date.now();
	const rows: string[] = [];
	visible.forEach((task, index) => {
		const last = index === visible.length - 1 && hidden === 0;
		const tail = lastLine(manager.outputTail(task.id)) || "waiting for output…";
		rows.push(
			`${theme.fg("dim", last ? "└─" : "├─")} ${theme.fg("accent", spinnerFrame(now))} ${theme.fg("text", oneLine(task.command, 160))} ${theme.fg("dim", `· ${formatClock(now - task.createdAt)}`)}`,
			theme.fg("dim", `${last ? "   " : "│  "}  ⎿  ${tail}`),
		);
	});
	if (hidden > 0) rows.push(theme.fg("dim", `└─ +${hidden} more running`));
	return rows;
}

function resultText(result: { content: { type: string; text?: string }[]; details?: unknown }, expanded: boolean, theme: Theme, action: string | undefined, isError: boolean): string {
	const text = result.content.filter((item) => item.type === "text").map((item) => item.text ?? "").join("\n");
	const details = (result.details ?? {}) as { task?: BackgroundTaskSummary; tasks?: BackgroundTaskSummary | BackgroundTaskSummary[]; text?: string };
	if (isError) return indent(theme.fg("error", expanded ? sanitizeTerminalText(text) : oneLine(text, 320)));
	if (action === "start") return "";
	if (action === "cancel" && details.task) return indent(`${stateMark(details.task, theme)} ${theme.fg("muted", stateWords(details.task))}`);
	if (action === "status" && details.tasks !== undefined) {
		const tasks = Array.isArray(details.tasks) ? details.tasks : [details.tasks];
		if (tasks.length === 0) return indent(theme.fg("muted", "No background commands."));
		return tasks.map((task) => indent(`${stateMark(task, theme)} ${theme.fg("muted", `${stateWords(task)} · ${oneLine(task.command, 100)}`)}`)).join("\n");
	}
	if (action === "output" && typeof details.text === "string") {
		const lines = sanitizeTerminalText(details.text).replace(/\r\n?/g, "\n").trimEnd().split("\n");
		if (lines.length === 1 && lines[0] === "") return indent(theme.fg("muted", "no output yet"));
		const shown = expanded ? lines : lines.slice(-OUTPUT_PREVIEW_LINES);
		const skipped = lines.length - shown.length;
		const hint = skipped > 0 ? [indent(theme.fg("muted", `… (${skipped} earlier line${skipped === 1 ? "" : "s"})`))] : [];
		return [...hint, ...shown.map((line) => indent(theme.fg("toolOutput", line)))].join("\n");
	}
	return indent(theme.fg("muted", expanded ? sanitizeTerminalText(text) : oneLine(text, 320)));
}

function indent(line: string): string {
	return line ? `  ${line}` : line;
}

function stateMark(task: BackgroundTaskSummary, theme: Theme): string {
	if (task.state === "running") return theme.fg("accent", "●");
	return theme.fg(stateColor(task), task.state === "succeeded" ? "✓" : task.state === "cancelled" || task.state === "interrupted" ? "■" : "✗");
}

function stateColor(task: BackgroundTaskSummary): "accent" | "success" | "error" | "warning" {
	if (task.state === "running") return "accent";
	if (task.state === "succeeded") return "success";
	if (task.state === "cancelled" || task.state === "interrupted") return "warning";
	return "error";
}

function stateWords(task: BackgroundTaskSummary): string {
	const clock = formatClock((task.finishedAt ?? Date.now()) - task.createdAt);
	switch (task.state) {
		case "running": return `running for ${clock}`;
		case "succeeded": return `finished in ${clock}`;
		case "failed": return `failed in ${clock}${task.exitCode === undefined || task.exitCode === null ? "" : ` (exit ${task.exitCode})`}`;
		case "timed_out": return `timed out after ${clock}`;
		case "cancelled": return `cancelled after ${clock}`;
		case "interrupted": return "interrupted by a restart";
	}
}

function lastLine(text: string): string {
	return text.split(/\r\n|\r|\n/).map((line) => oneLine(line, 200)).filter(Boolean).at(-1) ?? "";
}

function formatClock(ms: number): string {
	const seconds = Math.max(0, Math.floor(ms / 1000));
	if (seconds < 60) return `${seconds}s`;
	const minutes = Math.floor(seconds / 60);
	if (minutes < 60) return `${minutes}m ${seconds % 60}s`;
	return `${Math.floor(minutes / 60)}h ${minutes % 60}m`;
}

function completionText(tasks: BackgroundTaskSummary[]): string {
	const descriptions = tasks.map((task) => `- ${plainMark(task)} ${task.id} ${stateWords(task)} · ${oneLine(task.command, 160)}`).join("\n");
	return `Background command completion event\n${descriptions}\nRead each output with ct_background(action="output", taskId="...") before reporting.`;
}

function plainMark(task: BackgroundTaskSummary): string {
	if (task.state === "succeeded") return "✓";
	return task.state === "cancelled" || task.state === "interrupted" ? "■" : "✗";
}

function toolResult(text: string, details: unknown) {
	return { content: [{ type: "text" as const, text }], details };
}

function oneLine(text: string, maxLength: number): string {
	const value = sanitizeTerminalText(text).replace(/[\r\n\t]+/g, " ").replace(/\s+/g, " ").trim();
	return value.length <= maxLength ? value : `${value.slice(0, maxLength - 1)}…`;
}

function sanitizeTerminalText(text: string): string {
	return stripVTControlCharacters(text).replace(/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f\u061c\u200e\u200f\u202a-\u202e\u2066-\u206f]/g, "");
}

function formatTask(task: BackgroundTaskSummary): string {
	const duration = Math.max(0, Math.floor(((task.finishedAt ?? Date.now()) - task.createdAt) / 1000));
	return `${task.id} · ${task.state} · ${duration}s${task.exitCode === undefined ? "" : ` · exit ${task.exitCode}`}\n${oneLine(task.command, 200)}`;
}
