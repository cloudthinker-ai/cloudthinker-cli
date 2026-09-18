import { InteractiveMode } from "@earendil-works/pi-coding-agent";
import type { Component } from "@earendil-works/pi-tui";
import type { ToolDefinition } from "@earendil-works/pi-coding-agent";

import { LOCAL_TAG, LOCAL_TOOLS, createLegend, taggedComponent, type Legend } from "@cloudthinker/pi/src/awareness.ts";

type RenderCall = (args: any, theme: any, context: any) => Component;

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
	} as ToolDefinition;
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
