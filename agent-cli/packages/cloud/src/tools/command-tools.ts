import type { TSchema } from "typebox";

import type { CommandTool, CommandToolResult } from "../client.ts";
import type { CloudThinkerRuntime } from "../runtime.ts";
import { addCloudTools, CLOUD_TOOLS } from "./names.ts";
import { callComponent, callLine, firstLine, resultBody, summaryComponent } from "./render.ts";
import { text } from "./shared.ts";

const registered = new Set<string>();

export async function registerCommandTools(runtime: CloudThinkerRuntime): Promise<string[]> {
	const tools = await runtime.client.listCommandTools();
	if (!tools) return [];
	const added: string[] = [];
	for (const tool of tools) {
		if (tool.kind !== "read") continue;
		if (CLOUD_TOOLS.includes(tool.name) && !registered.has(tool.name)) continue;
		registerCommandTool(runtime, tool);
		registered.add(tool.name);
		added.push(tool.name);
	}
	addCloudTools(added);
	if (!runtime.cloudEnabled) {
		runtime.pi.setActiveTools(runtime.pi.getActiveTools().filter((name) => !added.includes(name)));
	}
	return added;
}

function registerCommandTool(runtime: CloudThinkerRuntime, tool: CommandTool): void {
	runtime.pi.registerTool<TSchema, CommandToolResult>({
		name: tool.name,
		label: tool.title || tool.name,
		description: tool.description,
		promptSnippet: firstLine(tool.description),
		parameters: tool.input_schema as unknown as TSchema,
		execute: async (_toolCallId, params, signal) => {
			runtime.requireSession();
			const result = await runtime.client.callCommandTool(tool.name, params as Record<string, unknown>, signal);
			if (result.error) throw new Error(result.text);
			return text(result.text, result);
		},
		renderCall: (params, theme) =>
			callComponent(callLine(theme, tool.name, summarizeArgs(params as Record<string, unknown>))),
		renderResult: (result, options, theme) =>
			summaryComponent(theme, firstLine(resultBody(result)), resultBody(result), options.expanded),
	});
}

function summarizeArgs(params: Record<string, unknown>): string {
	return Object.entries(params ?? {})
		.map(([key, value]) => `${key}=${typeof value === "string" ? value : JSON.stringify(value)}`)
		.join(" ");
}
