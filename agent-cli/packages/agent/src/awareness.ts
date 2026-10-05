import { InteractiveMode } from "@earendil-works/pi-coding-agent";
import { Box, Container, Text, type Component, sliceByColumn, visibleWidth } from "@earendil-works/pi-tui";
import type { ToolDefinition } from "@earendil-works/pi-coding-agent";

import { LOCAL_TAG, LOCAL_TOOLS, sanitizeTerminalText, taggedComponent } from "@cloudthinker/cloud/src/awareness.ts";
import { formatElapsed } from "@cloudthinker/cloud/src/tools/render.ts";

import { renderToolPath } from "../node_modules/@earendil-works/pi-coding-agent/dist/core/tools/render-utils.js";
import { staysInForeground } from "./background/shell.ts";
import { CommandBlock } from "./command-block.ts";
import { DiffView, hunksFromContent, parsePiDiff, type Hunk } from "./diff-view.ts";
import { diffStyle, toolOutputMode } from "./verbosity.ts";

type RenderCall = (args: any, theme: any, context: any) => Component;
type RenderResult = (result: any, options: any, theme: any, context: any) => Component;

export const COMPACT_TOOLS: string[] = ["bash", "powershell", "read", "grep", "find", "ls"];
export const CHANGE_TOOLS: string[] = ["edit", "write"];
export const SHELL_TOOLS: string[] = ["bash", "powershell"];
export const STOPPED_MARK = "Command aborted";
export const pointer = { available: true };
const EXPAND_HINT = /ctrl\+o((?:\x1b\[[0-9;]*m)*) to expand/;

export function diffStats(diff: string): { added: number; removed: number } {
	const lines = diff.split("\n");
	return { added: lines.filter((line) => line.startsWith("+")).length, removed: lines.filter((line) => line.startsWith("-")).length };
}

export function changeLine(toolName: string, args: any, theme: any, context: any): string {
	const path = renderToolPath(typeof args?.file_path === "string" ? args.file_path : typeof args?.path === "string" ? args.path : "", theme, context.cwd);
	const head = `${theme.fg("toolTitle", theme.bold(toolName))} ${path}`;
	if (toolName === "write") {
		const count = typeof args?.content === "string" ? args.content.replace(/\n$/, "").split("\n").length : 0;
		return count > 0 ? `${head}  ${theme.fg("success", `+${count}`)}` : head;
	}
	const diff = context.state?.callComponent?.preview?.diff;
	if (typeof diff !== "string") return head;
	const { added, removed } = diffStats(diff);
	return `${head}  ${theme.fg("success", `+${added}`)} ${theme.fg("error", `−${removed}`)}`;
}

function collapsedChange(toolName: string, args: any, theme: any, context: any, tag: (component: Component) => Component): Component | undefined {
	if (!CHANGE_TOOLS.includes(toolName) || toolOutputMode() !== "compact" || context.expanded || context.isError) return undefined;
	const line = tag(new Text(changeLine(toolName, args, theme, context), 0, 0));
	if (toolName !== "edit") return line;
	const box = new Box(1, 1, (text: string) => theme.bg(context.isPartial ? "toolPendingBg" : "toolSuccessBg", text));
	box.addChild(line);
	return box;
}

const DIFF_VIEW_CACHE = 64;
const diffViews = new Map<string, { source: string; theme: unknown; view: DiffView | undefined }>();

function detailedView(toolName: string, args: any, theme: any, context: any, tag: (component: Component) => Component): Component | undefined {
	if (context.isError || (toolOutputMode() !== "preview" && !context.expanded)) return undefined;
	if (SHELL_TOOLS.includes(toolName)) {
		const command = typeof args?.command === "string" ? args.command : "";
		if (!command.trim()) return undefined;
		return tag(new CommandBlock(command, toolName === "powershell" ? "PS>" : "$", theme, { fold: !context.expanded, indent: visibleWidth(LOCAL_TAG) + 1 }));
	}
	if (!CHANGE_TOOLS.includes(toolName)) return undefined;
	const diff = context.state?.callComponent?.preview?.diff;
	const source = toolName === "edit" ? diff : args?.content;
	if (typeof source !== "string") return undefined;
	const path = typeof args?.file_path === "string" ? args.file_path : typeof args?.path === "string" ? args.path : undefined;
	const key = `${String(context.toolCallId)}:${context.expanded ? "full" : "preview"}`;
	let cached = diffViews.get(key);
	if (cached?.source !== source || cached.theme !== theme) {
		const hunks: Hunk[] = toolName === "edit" ? parsePiDiff(source) : hunksFromContent(source);
		cached = { source, theme, view: hunks.length > 0 ? new DiffView(hunks, theme, { preview: !context.expanded, style: diffStyle, path, wholeFile: toolName === "write" }) : undefined };
		diffViews.delete(key);
		diffViews.set(key, cached);
		if (diffViews.size > DIFF_VIEW_CACHE) diffViews.delete(diffViews.keys().next().value!);
	}
	const view = cached.view;
	if (!view) return undefined;
	const header = tag(new Text(changeLine(toolName, args, theme, context), 0, 0));
	if (toolName !== "edit") {
		const stack = new Container();
		stack.addChild(header);
		stack.addChild(view);
		return stack;
	}
	const box = new Box(1, 1);
	box.addChild(header);
	box.addChild(view);
	return box;
}

interface ToolRendererHost {
	getRegisteredToolDefinition(toolName: string): ToolDefinition | undefined;
}

export function tagLocalToolDefinition(
	toolName: string,
	definition: ToolDefinition | undefined,
): ToolDefinition | undefined {
	if (!definition || !LOCAL_TOOLS.includes(toolName) || typeof definition.renderCall !== "function") {
		return definition;
	}
	const renderCall = definition.renderCall as RenderCall;
	let lastComponent: Component | undefined;
	const commandLine = new CommandLine();
	return {
		...definition,
		renderCall: (args: any, theme: any, context: any) => {
			const inner = renderCall(args, theme, { ...context, lastComponent });
			lastComponent = inner;
			const tag = (component: Component) => taggedComponent(LOCAL_TAG, theme, component);
			return collapsedChange(toolName, args, theme, context, tag) ?? detailedView(toolName, args, theme, context, tag) ?? tag(shortCommand(toolName, args, theme, context, commandLine) ?? clickToExpand(inner));
		},
		...(COMPACT_TOOLS.includes(toolName) && typeof definition.renderResult === "function"
			? { renderResult: compactResult(definition.renderResult as RenderResult, toolName === "bash") }
			: {}),
	} as ToolDefinition;
}

function compactResult(renderResult: RenderResult, movable: boolean): RenderResult {
	return (result, options, theme, context) => {
		const state = (context?.state ?? {}) as { ctInner?: Component; ctSummary?: Text; startedAt?: number; endedAt?: number };
		const inner = renderResult(result, options, theme, { ...context, lastComponent: state.ctInner });
		state.ctInner = inner;
		const stopped = context?.isError && resultText(result).endsWith(STOPPED_MARK);
		if (toolOutputMode() !== "compact" || options?.expanded || (context?.isError && !stopped)) return clickToExpand(inner);
		const summary = state.ctSummary ?? new Text("", 0, 0);
		state.ctSummary = summary;
		summary.setText(stopped ? stoppedLine(result, theme, state) : summaryLine(result, theme, state, Boolean(options?.isPartial), movable && !staysInForeground(String(context?.args?.command ?? ""))));
		return summary;
	};
}

function resultText(result: any): string {
	return (result?.content ?? [])
		.filter((item: { type?: string }) => item.type === "text")
		.map((item: { text?: string }) => item.text ?? "")
		.join("\n")
		.trimEnd();
}

function stoppedLine(result: any, theme: any, state: { startedAt?: number; endedAt?: number }): string {
	const count = resultText(result).slice(0, -STOPPED_MARK.length).trimEnd().split("\n").filter(Boolean).length;
	const parts = [`${count} line${count === 1 ? "" : "s"}`];
	if (state.startedAt !== undefined && state.endedAt !== undefined) parts.push(formatElapsed(state.endedAt - state.startedAt));
	return `    ${theme.fg("warning", "stopped")} ${theme.fg("muted", `· ${parts.join(" · ")}`)}`;
}

export class CommandLine implements Component {
	private head = "";
	private style: (text: string) => string = (text) => text;
	private suffix = "";

	set(head: string, style: (text: string) => string, suffix: string): this {
		this.head = head;
		this.style = style;
		this.suffix = suffix;
		return this;
	}

	render(width: number): string[] {
		const room = Math.max(10, width - (this.suffix ? visibleWidth(this.suffix) + 1 : 0));
		const head = visibleWidth(this.head) <= room ? this.head : `${sliceByColumn(this.head, 0, room - 3)}...`;
		return [`${this.style(head)}${this.suffix ? ` ${this.suffix}` : ""}`];
	}

	invalidate(): void {}
}

function shortCommand(toolName: string, args: any, theme: any, context: any, line: CommandLine): Component | undefined {
	const command = typeof args?.command === "string" ? args.command.trim() : "";
	if (!SHELL_TOOLS.includes(toolName) || !command || toolOutputMode() !== "compact" || context.expanded) return undefined;
	const lines = command.split("\n");
	const prompt = toolName === "powershell" ? "PS>" : "$";
	return line.set(`${prompt} ${sanitizeTerminalText(lines[0]!)}`, (text) => theme.fg("toolTitle", theme.bold(text)), lines.length > 1 ? theme.fg("muted", `+${lines.length - 1} lines`) : "");
}

export function clickToExpand(component: Component): Component {
	return {
		render: (width) => component.render(width).map((line) => line.replace(EXPAND_HINT, pointer.available ? "click$1 to expand" : "ctrl+o$1 for the transcript")),
		invalidate: () => component.invalidate(),
	};
}

function summaryLine(result: any, theme: any, state: { startedAt?: number; endedAt?: number }, running: boolean, movable = false): string {
	const text = resultText(result);
	const count = text ? text.split("\n").length : 0;
	const lines = `${count} line${count === 1 ? "" : "s"}`;
	if (running) return `    ${theme.fg("muted", `⋯ running${count === 0 ? "" : ` · ${lines}`}`)}${movable ? theme.fg("dim", " · ctrl+b to background") : ""}`;
	const moved = result?.details?.backgroundTaskId;
	if (typeof moved === "string") return `    ${theme.fg("accent", "⇢")} ${theme.fg("muted", `moved to background · ${moved}`)}`;
	const parts = [count === 0 ? "no output" : lines];
	if (state.startedAt !== undefined && state.endedAt !== undefined) parts.push(formatElapsed(state.endedAt - state.startedAt));
	return `    ${theme.fg("muted", parts.join(" · "))}`;
}

export function applyAwarenessUi(): void {
	const prototype = InteractiveMode.prototype as unknown as ToolRendererHost;
	const original = prototype.getRegisteredToolDefinition;
	if (typeof original !== "function") {
		throw new Error("pi's tool renderer seam changed, so local tool calls cannot be tagged [L]");
	}
	prototype.getRegisteredToolDefinition = function (toolName) {
		return tagLocalToolDefinition(toolName, original.call(this, toolName));
	};
}
