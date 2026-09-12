import {
	DefaultResourceLoader,
	SessionManager,
	createAgentSession,
	getAgentDir,
	type CreateAgentSessionOptions,
	type ExtensionAPI,
	type ExtensionContext,
	type ToolDefinition,
} from "@earendil-works/pi-coding-agent";
import subagents from "@tintinweb/pi-subagents/dist/index.js";
import { setSubagentHost, type SubagentHost } from "@tintinweb/pi-subagents/dist/host.js";
import { DEFAULT_AGENTS } from "@tintinweb/pi-subagents/dist/default-agents.js";

import cloudthinker from "@cloudthinker/pi/src/index.ts";
import { PROVIDER_ID } from "@cloudthinker/pi/src/provider.ts";
import { CLOUD_ENTRY_TYPE } from "@cloudthinker/pi/src/runtime.ts";
import { findLinkedSession } from "@cloudthinker/pi/src/session.ts";
import { CLOUD_TOOLS } from "@cloudthinker/pi/src/tools/names.ts";

type DefaultResourceLoaderOptions = ConstructorParameters<typeof DefaultResourceLoader>[0];

export const resolveCloudMode: SubagentHost["resolveModel"] = (input, registry) => {
	const modes = (registry.getAll?.() ?? registry.getAvailable?.() ?? [])
		.filter((model) => model.provider === PROVIDER_ID);
	const id = input.startsWith(`${PROVIDER_ID}/`) ? input.slice(PROVIDER_ID.length + 1) : input;
	const found = modes.find((model) => model.id === id);
	return found ?? `Choose a CloudThinker agent mode: ${modes.map((model) => `${PROVIDER_ID}/${model.id}`).join(", ") || "unavailable; check authentication and restart"}.`;
};

function requireCloudMode(model: Pick<NonNullable<CreateAgentSessionOptions["model"]>, "provider" | "id"> | undefined, ctx: ExtensionContext) {
	const resolved = resolveCloudMode(model ? `${model.provider}/${model.id}` : "", ctx.modelRegistry);
	if (typeof resolved === "string") throw new Error(resolved);
	return resolved;
}

function cloudEnabled(ctx: ExtensionContext): boolean {
	const entry = ctx.sessionManager.getEntries().findLast((item) => item.type === "custom" && item.customType === CLOUD_ENTRY_TYPE);
	return entry?.type !== "custom" || (entry.data as { enabled?: boolean } | undefined)?.enabled !== false;
}

export function childLoaderOptions(
	options: DefaultResourceLoaderOptions,
	ctx: ExtensionContext,
	includeCloudTools = true,
): DefaultResourceLoaderOptions {
	const entries = ctx.sessionManager.getEntries();
	const sourceConversationId = findLinkedSession(entries)?.conversation_id;
	const childOptions = { cloudEnabled: cloudEnabled(ctx) && includeCloudTools, sourceConversationId };
	return {
		...options,
		extensionFactories: [{
			name: "cloudthinker",
			factory: (pi) => cloudthinker(pi, childOptions),
		}],
		extensionsOverride: (base) => {
			const filtered = options.extensionsOverride?.(base) ?? base;
			const cloud = base.extensions.find((extension) => extension.path === "<inline:cloudthinker>");
			if (cloud && !childOptions.cloudEnabled) cloud.tools.clear();
			return {
				...filtered,
				extensions: [...new Set([
					...filtered.extensions,
					...base.extensions.filter((extension) => extension.path === "<inline:cloudthinker>"),
				])],
			};
		},
	};
}

export async function createCloudChild(options: CreateAgentSessionOptions, ctx: ExtensionContext) {
	const saved = options.sessionManager?.buildSessionContext().model;
	const model = requireCloudMode(saved ? { provider: saved.provider, id: saved.modelId } : options.model ?? ctx.model, ctx);
	let resourceLoader = options.resourceLoader;
	const needsBinding = !resourceLoader;
	if (!resourceLoader) {
		resourceLoader = new DefaultResourceLoader(childLoaderOptions({
			cwd: options.cwd ?? ctx.cwd,
			agentDir: getAgentDir(),
			noExtensions: true,
			noSkills: true,
			noPromptTemplates: true,
			noThemes: true,
			noContextFiles: true,
		}, ctx));
		await resourceLoader.reload();
	}
	const result = await createAgentSession({
		...options,
		model,
		resourceLoader,
		excludeTools: [...new Set([...(options.excludeTools ?? []), ...(cloudEnabled(ctx) ? [] : CLOUD_TOOLS)])],
		customTools: options.customTools?.map(cloudDelegationTool),
		sessionManager: options.sessionManager?.getSessionFile()
			? options.sessionManager
			: SessionManager.create(options.cwd ?? ctx.cwd, undefined, { parentSession: ctx.sessionManager.getSessionFile() }),
	});
	const stream = result.session.agent.streamFunction;
	result.session.agent.streamFunction = (selected, context, streamOptions) => {
		requireCloudMode(selected, ctx);
		return stream(selected, context, streamOptions);
	};
	if (needsBinding) await result.session.bindExtensions({});
	return result;
}

const adaptedTools = new WeakSet<ToolDefinition>();

export function cloudDelegationTool(tool: ToolDefinition): ToolDefinition {
	if (adaptedTools.has(tool) || !["Agent", "SubagentWorkflow"].includes(tool.name)) return tool;
	const schema = tool.parameters as typeof tool.parameters & { properties?: Record<string, { description?: string }> };
	const properties = { ...schema.properties };
	delete properties.thinking;
	if (properties.model) {
		properties.model = { ...properties.model, description: "CloudThinker agent mode ID, such as cloudthinker/pro. Omit to inherit the parent mode." };
	}
	if (properties.run_in_background) {
		properties.run_in_background = { ...properties.run_in_background, description: "Run in the background in interactive sessions. Print and JSON runs always wait for completion." };
	}
	tool.parameters = { ...tool.parameters, properties };
	const replacements: [string | RegExp, string][] = tool.name === "Agent" ? [
		['- Use model to specify a different model (as "provider/modelId", or fuzzy e.g. "haiku", "sonnet").', '- Use model only to select an advertised CloudThinker agent mode. Omit it to inherit the parent mode.'],
		['- Use thinking to control extended thinking level.\n', ''],
	] : [
		['effort?: string, ', ''],
		[/opts\.effort overrides .*?opts\.isolation:/s, 'opts.isolation:'],
		['agentType, model, effort, isolation', 'agentType, model, isolation'],
	];
	let description = tool.description;
	for (const [pattern, replacement] of replacements) {
		const rewritten = description.replace(pattern, replacement);
		if (rewritten === description) throw new Error(`Unsupported upstream ${tool.name} description; update the CloudThinker adapter.`);
		description = rewritten;
	}
	tool.description = description + "\nIn print and JSON mode, this tool waits for delegated work to complete and returns its results before the CLI exits.";
	adaptedTools.add(tool);
	return tool;
}

export default function bundledSubagents(pi: ExtensionAPI): ReturnType<typeof subagents> {
	setSubagentHost({
		resolveModel: resolveCloudMode,
		loaderOptions: childLoaderOptions,
		createSession: createCloudChild,
		modelChoices: (ctx) => ctx.modelRegistry.getAll().filter((model) => model.provider === PROVIDER_ID).map((model) => `${PROVIDER_ID}/${model.id}`),
	});
	for (const agent of DEFAULT_AGENTS.values()) {
		delete agent.model;
		delete agent.thinking;
	}
	pi.on("before_agent_start", (event, ctx) => ({
		systemPrompt: `${event.systemPrompt}\n\nSubagents use CloudThinker agent modes only. Available modes: ${ctx.modelRegistry.getAll().filter((model) => model.provider === PROVIDER_ID).map((model) => `${PROVIDER_ID}/${model.id}`).join(", ")}. Omit model to inherit the parent mode. The gateway manages reasoning effort.`,
	}));
	return subagents(new Proxy(pi, {
		get(target, key, receiver) {
			if (key === "registerTool") return (tool: ToolDefinition) => target.registerTool(cloudDelegationTool(tool));
			return Reflect.get(target, key, receiver);
		},
	}));
}
