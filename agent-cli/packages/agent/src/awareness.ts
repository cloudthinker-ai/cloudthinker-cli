import { InteractiveMode } from "@earendil-works/pi-coding-agent";
import { Text, type Component } from "@earendil-works/pi-tui";
import type { ToolDefinition } from "@earendil-works/pi-coding-agent";

import { LOCAL_TAG, LOCAL_TOOLS, createLegend, taggedComponent, type Legend } from "@cloudthinker/pi/src/awareness.ts";
import { formatElapsed } from "@cloudthinker/pi/src/tools/render.ts";

import { toolOutputMode } from "./verbosity.ts";

type RenderCall = (args: any, theme: any, context: any) => Component;
type RenderResult = (result: any, options: any, theme: any, context: any) => Component;

export const COMPACT_TOOLS: string[] = ["bash", "powershell", "read", "grep", "find", "ls"];

interface ToolRendererHost {
	getRegisteredToolDefinition(toolName: string): ToolDefinition | undefined;
}

const legends = new WeakMap<object, Legend>();

function legendFor(host: object): Legend {
	let legend = legends.get(host);
	if (!legend) {
		legend = createLegend();
		legends.set(host, legend);
	}
	return legend;
}

export function tagLocalToolDefinition(
	toolName: string,
	definition: ToolDefinition | undefined,
	legend: Legend = createLegend(),
): ToolDefinition | undefined {
	if (!definition || !LOCAL_TOOLS.includes(toolName) || typeof definition.renderCall !== "function") {
		return definition;
	}
	const renderCall = definition.renderCall as RenderCall;
	let lastComponent: Component | undefined;
	return {
		...definition,
		renderCall: (args: any, theme: any, context: any) => {
			const inner = renderCall(args, theme, { ...context, lastComponent });
			lastComponent = inner;
			return taggedComponent(LOCAL_TAG, theme, inner, legend.tag(context.toolCallId));
		},
		...(COMPACT_TOOLS.includes(toolName) && typeof definition.renderResult === "function"
			? { renderResult: compactResult(definition.renderResult as RenderResult) }
			: {}),
	} as ToolDefinition;
}

function compactResult(renderResult: RenderResult): RenderResult {
	return (result, options, theme, context) => {
		const state = (context?.state ?? {}) as { ctInner?: Component; ctSummary?: Text; startedAt?: number; endedAt?: number };
		const inner = renderResult(result, options, theme, { ...context, lastComponent: state.ctInner });
		state.ctInner = inner;
		if (toolOutputMode() !== "compact" || options?.expanded || options?.isPartial || context?.isError) return inner;
		const summary = state.ctSummary ?? new Text("", 0, 0);
		state.ctSummary = summary;
		summary.setText(summaryLine(result, theme, state));
		return summary;
	};
}

function summaryLine(result: any, theme: any, state: { startedAt?: number; endedAt?: number }): string {
	const text = (result?.content ?? [])
		.filter((item: { type?: string }) => item.type === "text")
		.map((item: { text?: string }) => item.text ?? "")
		.join("\n")
		.trimEnd();
	const count = text ? text.split("\n").length : 0;
	const parts = [count === 0 ? "no output" : `${count} line${count === 1 ? "" : "s"}`];
	if (state.startedAt !== undefined && state.endedAt !== undefined) parts.push(formatElapsed(state.endedAt - state.startedAt));
	return `    ${theme.fg("success", "✓")} ${theme.fg("muted", parts.join(" · "))}`;
}

export function applyAwarenessUi(): void {
	const prototype = InteractiveMode.prototype as unknown as ToolRendererHost;
	const original = prototype.getRegisteredToolDefinition;
	if (typeof original !== "function") {
		throw new Error("pi's tool renderer seam changed, so local tool calls cannot be tagged [L]");
	}
	prototype.getRegisteredToolDefinition = function (toolName) {
		return tagLocalToolDefinition(toolName, original.call(this, toolName), legendFor(this));
	};
}
