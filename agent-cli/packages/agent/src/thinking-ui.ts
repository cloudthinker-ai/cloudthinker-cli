import { SettingsManager } from "@earendil-works/pi-coding-agent";
import { Container, MouseRegion, Spacer } from "@earendil-works/pi-tui";

import { AssistantMessageComponent } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/components/assistant-message.js";
import { WorkingStatusIndicator } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/components/status-indicator.js";

export const ABORT_ERROR = "The operation was aborted.";

type Block = { type: string; thinking?: string };

export interface ThinkingRun {
	text: string;
	done: boolean;
}

interface AssistantHost {
	contentContainer: Container;
	isStreaming: boolean;
	hideThinkingBlock: boolean;
	thinkingVisibilityOverrides: Map<number, boolean>;
}

const starts = new WeakMap<object, Map<number, number>>();
let live: { host: AssistantHost; start: number } | undefined;

export function thinkingRuns(content: readonly Block[], isStreaming: boolean): ThinkingRun[] {
	const runs: ThinkingRun[] = [];
	for (let index = 0; index < content.length; index += 1) {
		if (content[index]!.type !== "thinking") continue;
		const texts: string[] = [];
		for (; index < content.length && content[index]!.type === "thinking"; index += 1) {
			const text = content[index]!.thinking?.trim();
			if (text) texts.push(text);
		}
		const done = !isStreaming || index < content.length;
		index -= 1;
		if (texts.length > 0) runs.push({ text: texts.join("\n\n"), done });
	}
	return runs;
}

export function formatThoughtTime(elapsedMs: number): string {
	const seconds = Math.max(1, Math.round(elapsedMs / 1000));
	if (seconds < 60) return `${seconds}s`;
	const minutes = Math.floor(seconds / 60);
	return seconds % 60 === 0 ? `${minutes}m` : `${minutes}m ${seconds % 60}s`;
}

export function thinkingStatus(message: string, now = Date.now()): string {
	if (!live?.host.isStreaming || !/^Working\b/.test(message)) return message;
	return message.replace(/^Working/, `Thinking ${formatThoughtTime(now - live.start)}`);
}

function withoutBlankRuns(children: object[]): object[] {
	const kept: object[] = [];
	for (const child of children) {
		if (child instanceof Spacer && kept.at(-1) instanceof Spacer) continue;
		kept.push(child);
	}
	return kept.some((child) => !(child instanceof Spacer)) ? kept : [];
}

function hideThinking(host: AssistantHost, content: readonly Block[]): void {
	const runs = thinkingRuns(content, host.isStreaming);
	const children = host.contentContainer.children;
	const regions = children.filter((child): child is MouseRegion => child instanceof MouseRegion);
	if (regions.length !== runs.length) return;
	const timing = starts.get(host) ?? new Map<number, number>();
	starts.set(host, timing);
	let liveStart: number | undefined;
	runs.forEach((run, index) => {
		if (host.isStreaming && !run.done) {
			liveStart = timing.get(index) ?? Date.now();
			timing.set(index, liveStart);
		}
		if (!(host.thinkingVisibilityOverrides.get(index) ?? host.hideThinkingBlock)) return;
		children.splice(children.indexOf(regions[index]!), 1);
	});
	if (liveStart !== undefined) live = { host, start: liveStart };
	else if (live?.host === host) live = undefined;
	children.splice(0, children.length, ...(withoutBlankRuns(children) as typeof children));
}

export function applyThinkingUi(): void {
	const settings = SettingsManager.prototype as unknown as { getHideThinkingBlock(this: { settings?: { hideThinkingBlock?: boolean } }): boolean };
	if (typeof settings.getHideThinkingBlock !== "function") {
		throw new Error("pi's SettingsManager no longer defines getHideThinkingBlock, so thinking cannot start collapsed");
	}
	settings.getHideThinkingBlock = function () {
		return this.settings?.hideThinkingBlock ?? true;
	};
	const assistant = AssistantMessageComponent.prototype as unknown as {
		updateContent(this: AssistantHost, message: { content: readonly Block[]; stopReason?: string; errorMessage?: string }, isStreaming?: boolean): void;
	};
	const updateContent = assistant.updateContent;
	assistant.updateContent = function (message, isStreaming) {
		const stopped = message.stopReason === "error" && message.errorMessage === ABORT_ERROR;
		updateContent.call(this, stopped ? { ...message, stopReason: "aborted", errorMessage: undefined } : message, isStreaming);
		hideThinking(this, message.content);
	};
	const indicator = WorkingStatusIndicator.prototype as unknown as { message: string; updateDisplay(this: { message: string }): void };
	const updateDisplay = indicator.updateDisplay;
	if (typeof updateDisplay !== "function") {
		throw new Error("pi's working indicator no longer redraws through updateDisplay, so live thinking cannot show there");
	}
	indicator.updateDisplay = function () {
		const message = this.message;
		this.message = thinkingStatus(message);
		try {
			updateDisplay.call(this);
		} finally {
			this.message = message;
		}
	};
	const probe = new AssistantMessageComponent(
		{ role: "assistant", content: [{ type: "thinking", thinking: "a" }, { type: "text", text: "b" }, { type: "thinking", thinking: "c" }] } as never,
		false,
		{} as never,
	) as unknown as AssistantHost;
	if (probe.contentContainer.children.filter((child) => child instanceof MouseRegion).length !== 2) {
		throw new Error("pi no longer wraps each thinking run in a MouseRegion, so finished thinking cannot be hidden");
	}
}
