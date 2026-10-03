import { resolve } from "node:path";
import { stripVTControlCharacters } from "node:util";
import { Type } from "typebox";
import { isKeyRelease, matchesKey, Text, truncateToWidth, visibleWidth, type Component } from "@earendil-works/pi-tui";
import {
	createBashToolDefinition,
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
import { autoBackgroundSeconds, ForegroundShells, movableShell, staysInForeground } from "./shell.ts";
import { formatClock, tasksPane } from "../tasks-pane.ts";
import { toolOutputMode } from "../verbosity.ts";
import { UserMessageComponent } from "../../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/components/user-message.js";
import { theme as uiTheme } from "../../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/theme/theme.js";

const TOOL_NAME = "ct_background";
const COMPLETION_HEADER = "Background command completion event";
const COMPLETION_ROW = /^- (\S+) (.+?) · (.*)$/;
const OUTPUT_PREVIEW_LINES = 5;
const OUTPUT_VIEW_BYTES = 8 * 1024;
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
	const keyListeners = new Map<string, () => void>();
	const foreground = new ForegroundShells();
	const sessionKey = (ctx: ExtensionContext) => ctx.sessionManager.getSessionId();
	const shells = new Map<string, { shellPath?: string; commandPrefix?: string }>();
	const shellSettings = (ctx: ExtensionContext) => {
		const key = sessionKey(ctx);
		let shell = shells.get(key);
		if (!shell) {
			const settings = SettingsManager.create(ctx.cwd, getAgentDir());
			shell = { shellPath: settings.getShellPath(), commandPrefix: settings.getShellCommandPrefix() };
			shells.set(key, shell);
		}
		return shell;
	};
	const renderStatus = (ctx: ExtensionContext, manager: BackgroundCommandManager) => {
		const tasks = manager.list();
		for (const task of tasks) commands.set(task.id, task.command);
		if (!ctx.hasUI) return;
		tasksPane.bind(ctx.ui);
		const running = tasks.filter((task) => task.state === "running");
		tasksPane.setCommands(running.map((task) => ({
			id: task.id,
			command: oneLine(task.command, 160),
			startedAt: task.createdAt,
			tail: () => lastLine(manager.outputTail(task.id)),
			output: () => {
				const current = manager.list().find((item) => item.id === task.id);
				return current ? manager.readOutput(task.id, Math.max(0, current.totalOutputBytes - OUTPUT_VIEW_BYTES)).text : "";
			},
			running: () => manager.list().some((item) => item.id === task.id && item.state === "running"),
			stop: async () => {
				if (manager.get(task.id).state !== "running") return;
				const stopped = await manager.cancel(task.id, true);
				if (stopped.stoppedByUser) deliver(ctx, [stopped]);
			},
		})));
		if (running.length > 0 && !statusTimers.has(sessionKey(ctx))) {
			statusTimers.set(sessionKey(ctx), setInterval(() => renderStatus(ctx, manager), 1000).unref());
		} else if (running.length === 0) {
			const timer = statusTimers.get(sessionKey(ctx));
			if (timer) clearInterval(timer);
			statusTimers.delete(sessionKey(ctx));
		}
	};
	const deliver = (ctx: ExtensionContext, tasks: BackgroundTaskSummary[]) => {
		if (ctx.mode === "tui" || ctx.mode === "rpc") {
			pi.sendUserMessage(completionText(tasks), { deliverAs: "followUp", expandPromptTemplates: false });
		} else {
			pi.sendMessage(completionMessage(tasks), { triggerTurn: true, deliverAs: "followUp" });
		}
	};
	const getManager = async (ctx: ExtensionContext): Promise<BackgroundCommandManager> => {
		const key = sessionKey(ctx);
		contexts.set(key, ctx);
		let managerPromise = managers.get(key);
		if (!managerPromise) {
			let manager!: BackgroundCommandManager;
			const { shellPath, commandPrefix } = shellSettings(ctx);
			const bash = createLocalBashOperations({ shellPath, cleanupOnExit: true });
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
					if (completed.length > 0) deliver(current, completed);
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
			const head = `${theme.fg("accent", "●")} ${theme.fg("toolTitle", theme.bold("Background"))}${verb} ${theme.fg("muted", oneLine(subject, 100))}`;
			const state = (_context?.state ?? {}) as { ctSuffix?: string };
			return {
				render: (width: number) => {
					if (!state.ctSuffix) return [truncateToWidth(head, width)];
					const tail = ` ${theme.fg("muted", "·")} ${state.ctSuffix}`;
					return [`${truncateToWidth(head, Math.max(1, width - visibleWidth(tail)))}${tail}`];
				},
				invalidate() {},
			} satisfies Component;
		},
		renderResult(result, options, theme, context) {
			const state = (context?.state ?? {}) as { ctSuffix?: string };
			const action = context?.args?.action;
			const isError = context?.isError === true;
			state.ctSuffix = toolOutputMode() === "compact" && !options.expanded && !isError ? summaryText(result, theme, action) : undefined;
			return new Text(state.ctSuffix === undefined ? resultText(result, options.expanded, theme, action, isError) : "", 0, 0);
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

	pi.registerTool({
		...createBashToolDefinition(process.cwd()),
		async execute(toolCallId, params: { command: string; timeout?: number }, signal, onUpdate, ctx) {
			const shell = shellSettings(ctx);
			const manager = ctx.mode === "tui" && !staysInForeground(params.command) ? await getManager(ctx).catch(() => undefined) : undefined;
			if (!manager) return createBashToolDefinition(ctx.cwd, shell).execute(toolCallId, params, signal, onUpdate, ctx);
			const timeoutSeconds = params.timeout ?? BACKGROUND_DEFAULT_TIMEOUT_SECONDS;
			let moved: BackgroundTaskSummary | undefined;
			const operations = movableShell({
				local: createLocalBashOperations({ shellPath: shell.shellPath, cleanupOnExit: true }),
				displayCommand: params.command,
				seconds: autoBackgroundSeconds(),
				foreground,
				adopt: (command) => manager.adopt(command, timeoutSeconds),
				onMoved: (task) => { moved = task; },
			});
			const result = await createBashToolDefinition(ctx.cwd, { operations, commandPrefix: shell.commandPrefix }).execute(toolCallId, params, signal, onUpdate, ctx);
			if (!moved) return result;
			const text = result.content.filter((item) => item.type === "text").map((item) => item.text).join("\n");
			return {
				content: [{ type: "text" as const, text: movedText(text, moved, timeoutSeconds) }],
				details: { ...(result.details ?? {}), backgroundTaskId: moved.id },
			};
		},
	});

	pi.on("session_start", async (_event, ctx) => {
		const manager = await getManager(ctx);
		renderStatus(ctx, manager);
		if (ctx.hasUI && ctx.mode === "tui") {
			keyListeners.get(sessionKey(ctx))?.();
			keyListeners.set(sessionKey(ctx), ctx.ui.onTerminalInput((data) => {
				if (isKeyRelease(data) || !matchesKey(data, "ctrl+b") || foreground.size === 0) return undefined;
				foreground.moveAll();
				return { consume: true };
			}));
		}
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
		keyListeners.get(key)?.();
		keyListeners.delete(key);
		contexts.delete(key);
		shells.delete(key);
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

function resultText(result: { content: { type: string; text?: string }[]; details?: unknown }, expanded: boolean, theme: Theme, action: string | undefined, isError: boolean): string {
	const text = result.content.filter((item) => item.type === "text").map((item) => item.text ?? "").join("\n");
	const details = (result.details ?? {}) as { task?: BackgroundTaskSummary; tasks?: BackgroundTaskSummary | BackgroundTaskSummary[]; text?: string };
	if (isError) return indent(theme.fg("error", expanded ? sanitizeTerminalText(text) : oneLine(text, 320)));
	if (action === "start") return "";
	if (action === "cancel" && details.task) return indent(stateLine(details.task, theme));
	if (action === "status" && details.tasks !== undefined) {
		const tasks = Array.isArray(details.tasks) ? details.tasks : [details.tasks];
		if (tasks.length === 0) return indent(theme.fg("muted", "No background commands."));
		return tasks.map((task) => indent(`${stateLine(task, theme)} ${theme.fg("muted", `· ${oneLine(task.command, 100)}`)}`)).join("\n");
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

function stateLine(task: BackgroundTaskSummary, theme: Theme): string {
	if (task.state === "running") return `${theme.fg("accent", "●")} ${theme.fg("muted", stateWords(task))}`;
	return theme.fg(task.state === "succeeded" ? "muted" : stateColor(task), stateWords(task));
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
		case "cancelled": return `${task.stoppedByUser ? "stopped by the user" : "cancelled"} after ${clock}`;
		case "interrupted": return "interrupted by a restart";
	}
}

function lastLine(text: string): string {
	return text.split(/\r\n|\r|\n/).map((line) => oneLine(line, 200)).filter(Boolean).at(-1) ?? "";
}

function movedText(output: string, task: BackgroundTaskSummary, timeoutSeconds: number): string {
	return `${output}\n\nStill running, so it moved to background command ${task.id}; it was not restarted and stops ${timeoutSeconds} seconds after it started. Continue other work: its completion event arrives on its own. Read its output with ct_background(action="output", taskId="${task.id}"), and do not run it again.`;
}

function completionText(tasks: BackgroundTaskSummary[]): string {
	const descriptions = tasks.map((task) => `- ${task.id} ${stateWords(task)} · ${oneLine(task.command, 160)}`).join("\n");
	return `${COMPLETION_HEADER}\n${descriptions}\nRead each output with ct_background(action="output", taskId="...") before reporting.`;
}

export function completionRows(text: string, theme: Pick<Theme, "fg" | "bold">): string[] | undefined {
	const [header, ...lines] = text.split("\n");
	if (header !== COMPLETION_HEADER) return undefined;
	const rows = lines.flatMap((line) => {
		const match = COMPLETION_ROW.exec(line);
		if (!match) return [];
		const words = match[2]!;
		const color = words.startsWith("finished") ? "success" : words.startsWith("stopped") || words.startsWith("cancelled") ? "warning" : "error";
		return [` ${theme.bold(match[3]!)} ${theme.fg("dim", "·")} ${theme.fg(color, words)}`];
	});
	return rows.length > 0 ? rows : undefined;
}

export function applyCompletionRows(): void {
	const prototype = UserMessageComponent.prototype as unknown as { text: string; render(width: number): string[] };
	const render = prototype.render;
	if (typeof render !== "function") throw new Error("pi's user message renderer changed, so background completions cannot render as one row");
	prototype.render = function (width) {
		const rows = typeof this.text === "string" ? completionRows(this.text, uiTheme) : undefined;
		return rows ? ["", ...rows.map((row) => truncateToWidth(row, width))] : render.call(this, width);
	};
}

function summaryText(result: { content: { type: string; text?: string }[]; details?: unknown }, theme: Theme, action: string | undefined): string | undefined {
	const details = (result.details ?? {}) as { task?: BackgroundTaskSummary; tasks?: BackgroundTaskSummary | BackgroundTaskSummary[]; text?: string };
	if (action === "cancel" && details.task) return stateLine(details.task, theme);
	if (action === "status" && details.tasks !== undefined) {
		if (!Array.isArray(details.tasks)) return stateLine(details.tasks, theme);
		const running = details.tasks.filter((task) => task.state === "running").length;
		if (details.tasks.length === 0) return theme.fg("muted", "no commands");
		return theme.fg("muted", `${details.tasks.length} command${details.tasks.length === 1 ? "" : "s"}${running > 0 ? ` · ${running} running` : ""}`);
	}
	if (action === "output" && typeof details.text === "string") {
		const text = details.text.replace(/\r\n?/g, "\n").trimEnd();
		const count = text ? text.split("\n").length : 0;
		return theme.fg("muted", count === 0 ? "no output yet" : `${count} line${count === 1 ? "" : "s"}`);
	}
	return undefined;
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
	return `${task.id} · ${task.stoppedByUser ? "stopped by the user" : task.state} · ${duration}s${task.exitCode === undefined ? "" : ` · exit ${task.exitCode}`}\n${oneLine(task.command, 200)}`;
}
