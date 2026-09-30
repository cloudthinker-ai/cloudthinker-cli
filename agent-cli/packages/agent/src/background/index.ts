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
} from "@earendil-works/pi-coding-agent";
import {
	BACKGROUND_DEFAULT_TIMEOUT_SECONDS,
	BackgroundCommandManager,
	type BackgroundTaskSummary,
} from "./manager.ts";

const TOOL_NAME = "ct_background";
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
	const sessionKey = (ctx: ExtensionContext) => ctx.sessionManager.getSessionId();
	const renderStatus = (ctx: ExtensionContext, manager: BackgroundCommandManager) => {
		if (!ctx.hasUI) return;
		const tasks = manager.list().filter((task) => task.state === "running");
		const visible = tasks.slice(-5);
		const lines = visible.map((task) => {
			const duration = Math.max(0, Math.floor(((task.finishedAt ?? Date.now()) - task.createdAt) / 1000));
			return `${task.state.padEnd(10)} ${duration}s  ${task.id}  ${oneLine(task.command, 84)}`;
		});
		const active = tasks.length;
		ctx.ui.setStatus("ct-background", active > 0 ? `${active} background command${active === 1 ? "" : "s"}` : undefined);
		ctx.ui.setWidget("ct-background", tasks.length > 0 ? ["Background commands", ...lines] : undefined);
		if (active > 0 && !statusTimers.has(sessionKey(ctx))) {
			statusTimers.set(sessionKey(ctx), setInterval(() => renderStatus(ctx, manager), 1000).unref());
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
		renderCall(args, _theme, _context) {
			const subject = args.action === "start" ? oneLine(args.command ?? "", 100) : args.taskId ?? "commands";
			return new Text(`Background command · ${args.action}: ${subject}`, 0, 0);
		},
		renderResult(result, options, _theme, _context) {
			const text = result.content.filter((item) => item.type === "text").map((item) => item.text).join("\n");
			return new Text(options.expanded ? sanitizeTerminalText(text) : oneLine(text, 320), 0, 0);
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

function completionText(tasks: BackgroundTaskSummary[]): string {
	const descriptions = tasks.map((task) => `- ${task.id}: ${task.state} (exit ${task.exitCode ?? "unknown"}) — ${oneLine(task.command, 160)}`).join("\n");
	return `Background command completion event. Read each task's output with ct_background(action="output", taskId="...") before reporting.\n${descriptions}`;
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
