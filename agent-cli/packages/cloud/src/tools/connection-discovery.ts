import { Type } from "typebox";

import type { CloudThinkerRuntime } from "../runtime.ts";
import { LOAD_SKILL, TOOL_SEARCH } from "./names.ts";
import { outputPreviewLines, resultBody, summaryComponent } from "./render.ts";
import { text } from "./shared.ts";

const searchParameters = Type.Object({
	query: Type.String({
		minLength: 1,
		maxLength: 300,
		description: "A short task to search for, or exact qualified tool names such as grafana.list_alerts.",
	}),
	limit: Type.Optional(
		Type.Integer({
			minimum: 1,
			maximum: 10,
			description: "Maximum number of search matches.",
		}),
	),
});

const skillParameters = Type.Object({
	connection_prefix: Type.String({
		minLength: 1,
		maxLength: 128,
		pattern: "^[a-zA-Z0-9_-]+$",
		description: "The connected prefix from the Connection list.",
	}),
	skill_names: Type.Optional(
		Type.Array(Type.String(), {
			maxItems: 20,
			description: "Advertised skill names. For an SDK Connection with no named skills, omit this to read its tool policy.",
		}),
	),
});

export function registerConnectionDiscovery(runtime: CloudThinkerRuntime): void {
	runtime.pi.registerTool<typeof searchParameters, undefined>({
		name: TOOL_SEARCH,
		label: "Find Connection tools",
		description: "Find tools on connected services by task, or load their schemas by exact name. Returns descriptions and input schemas without running the tools or changing anything.",
		parameters: searchParameters,
		execute: async (_toolCallId, params, signal) => {
			const { conversation_id } = runtime.requireSession();
			const result = await runtime.client.searchConnectionTools({ ...params, conversation_id }, signal);
			return text(result.content, undefined);
		},
		renderResult: (result, options, theme) =>
			summaryComponent(theme, "Connection tools", resultBody(result), options.expanded, outputPreviewLines(false)),
	});

	runtime.pi.registerTool<typeof skillParameters, undefined>({
		name: LOAD_SKILL,
		label: "Read Connection guide",
		description: `Read a Connection's advertised guides and check its tool policy before using it. For an SDK Connection without named guides, pass only its prefix. Use ${TOOL_SEARCH} for exact tool schemas. This loads instructions without running any commands.`,
		parameters: skillParameters,
		execute: async (_toolCallId, params, signal) => {
			const { conversation_id } = runtime.requireSession();
			const result = await runtime.client.loadConnectionSkill({ ...params, conversation_id }, signal);
			return text(result.content, undefined);
		},
		renderResult: (result, options, theme) =>
			summaryComponent(theme, "Connection guide", resultBody(result), options.expanded, outputPreviewLines(false)),
	});
}
