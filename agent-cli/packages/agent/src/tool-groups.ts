import { InteractiveMode } from "@earendil-works/pi-coding-agent";
import { Box, type Component, Container, Text, TuiAltScreen, type TuiMouseEvent, type TuiMouseEventResult } from "@earendil-works/pi-tui";

import { LOCAL_TAG, taggedComponent } from "@cloudthinker/cloud/src/awareness.ts";

import { AssistantMessageComponent } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/components/assistant-message.js";
import { ToolExecutionComponent } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/components/tool-execution.js";
import { theme } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/theme/theme.js";
import { pointer } from "./awareness.ts";
import { toolOutputMode } from "./verbosity.ts";

export const GROUP_TOOLS = ["read", "grep", "find", "ls"];

interface ToolLike {
	toolName: string;
	args: Record<string, unknown> | undefined;
	result?: { isError?: boolean };
	render(width: number): string[];
}

interface GroupState {
	members: ToolLike[];
	head: ToolLike;
}

const roles = new WeakMap<object, GroupState>();
const quiet = new WeakMap<object, GroupState>();

interface AssistantLike {
	lastMessage?: { content: readonly { type: string; text?: string }[]; stopReason?: string };
}

function onlyThinking(child: unknown): boolean {
	if (!(child instanceof AssistantMessageComponent)) return false;
	const message = (child as unknown as AssistantLike).lastMessage;
	if (!message) return false;
	if (message.stopReason === "error" || message.stopReason === "aborted" || message.stopReason === "length") return false;
	return message.content.every((block) => block.type !== "text" || !block.text?.trim());
}
const open = new WeakSet<object>();

function groupable(child: unknown): child is ToolLike {
	return child instanceof ToolExecutionComponent
		&& GROUP_TOOLS.includes((child as unknown as ToolLike).toolName)
		&& (child as unknown as ToolLike).result?.isError !== true;
}

function plural(count: number, one: string, many: string): string {
	return `${count} ${count === 1 ? one : many}`;
}

export function groupSummary(members: readonly Pick<ToolLike, "toolName" | "args" | "result">[]): string {
	const unique = (tool: string, key: string) =>
		new Set(members.filter((member) => member.toolName === tool).map((member) => String(member.args?.[key] ?? member.args?.file_path ?? ""))).size;
	const count = (tool: string) => members.filter((member) => member.toolName === tool).length;
	const parts = [
		count("read") > 0 ? `read ${plural(unique("read", "path"), "file", "files")}` : "",
		count("grep") > 0 ? `searched ${plural(count("grep"), "pattern", "patterns")}` : "",
		count("find") > 0 ? `looked up ${plural(count("find"), "file pattern", "file patterns")}` : "",
		count("ls") > 0 ? `listed ${plural(unique("ls", "path"), "folder", "folders")}` : "",
	].filter(Boolean);
	const text = parts.join(", ");
	const running = members.some((member) => member.result === undefined);
	return `${text.charAt(0).toUpperCase()}${text.slice(1)}${running ? "…" : ""}`;
}

export function assignGroups(children: readonly Component[], width: number): void {
	let run: ToolLike[] = [];
	let between: Component[] = [];
	let pending: Component[] = [];
	const close = () => {
		if (run.length >= 2) {
			const state = { members: run, head: run[0]! };
			for (const member of run) roles.set(member, state);
			for (const child of between) quiet.set(child, state);
		}
		run = [];
		between = [];
		pending = [];
	};
	for (const child of children) {
		roles.delete(child);
		quiet.delete(child);
		if (groupable(child)) {
			if (run.length > 0) between.push(...pending);
			pending = [];
			run.push(child);
		} else if (run.length > 0 && onlyThinking(child)) pending.push(child);
		else if (child instanceof ToolExecutionComponent || (run.length > 0 && child.render(width).length > 0)) close();
	}
	close();
}

function headerLines(state: GroupState, width: number): string[] {
	const isOpen = open.has(state.head);
	const running = state.members.some((member) => member.result === undefined);
	const line = new Text(`${theme.fg("muted", isOpen ? "▾" : "▸")} ${theme.fg("toolTitle", groupSummary(state.members))}`, 0, 0);
	const box = new Box(1, isOpen ? 0 : 1, (text: string) => theme.bg(running ? "toolPendingBg" : "toolSuccessBg", text));
	box.addChild(taggedComponent(LOCAL_TAG, theme, line));
	return ["", ...box.render(width)];
}

export function applyToolGroups(): void {
	const tool = ToolExecutionComponent.prototype as unknown as {
		render(this: ToolLike, width: number): string[];
		handleMouse(this: ToolLike, event: TuiMouseEvent): TuiMouseEventResult | undefined;
	};
	const { render, handleMouse } = tool;
	if (typeof render !== "function" || typeof handleMouse !== "function") {
		throw new Error("pi's ToolExecutionComponent no longer renders and handles the mouse, so tool runs cannot be grouped");
	}
	tool.render = function (width) {
		const state = toolOutputMode() === "compact" ? roles.get(this) : undefined;
		if (!state) return render.call(this, width);
		if (!open.has(state.head)) return state.head === this ? headerLines(state, width) : [];
		return state.head === this ? [...headerLines(state, width), ...render.call(this, width)] : render.call(this, width);
	};
	tool.handleMouse = function (event) {
		const state = toolOutputMode() === "compact" ? roles.get(this) : undefined;
		if (state?.head === this) {
			const header = headerLines(state, event.width).length;
			if (event.y < header) {
				if (event.type !== "click" || event.button !== "left") return { handled: true, render: false };
				if (open.has(state.head)) open.delete(state.head);
				else open.add(state.head);
				return { handled: true };
			}
			return handleMouse.call(this, { ...event, y: event.y - header, height: event.height - header });
		}
		return handleMouse.call(this, event);
	};
	const assistant = AssistantMessageComponent.prototype as unknown as { render(this: object, width: number): string[] };
	const renderAssistant = assistant.render;
	assistant.render = function (width) {
		const state = toolOutputMode() === "compact" ? quiet.get(this) : undefined;
		return state && !open.has(state.head) ? [] : renderAssistant.call(this, width);
	};
	const prototype = InteractiveMode.prototype as unknown as { setupKeyHandlers(this: { chatContainer: Container; ui: unknown }): void };
	const setupKeyHandlers = prototype.setupKeyHandlers;
	prototype.setupKeyHandlers = function () {
		setupKeyHandlers.call(this);
		pointer.available = this.ui instanceof TuiAltScreen;
		const chat = this.chatContainer;
		const renderChat = chat.render;
		chat.render = function (width: number) {
			if (toolOutputMode() === "compact") assignGroups(this.children, width);
			return renderChat.call(this, width);
		};
	};
}
