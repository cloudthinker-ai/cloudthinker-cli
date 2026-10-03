import {
	DefaultResourceLoader,
	SessionManager,
	createAgentSession,
	getAgentDir,
	type CreateAgentSessionOptions,
	type ExtensionAPI,
	type ExtensionContext,
	type MessageRenderer,
	type Theme,
	type ToolDefinition,
} from "@earendil-works/pi-coding-agent";
import { Text } from "@earendil-works/pi-tui";
import subagents from "@cloudthinker/subagents/src/index.ts";
import { setSubagentHost, type SubagentHost } from "@cloudthinker/subagents/src/host.ts";
import { DEFAULT_AGENTS } from "@cloudthinker/subagents/src/default-agents.ts";
import { workflowConcurrency } from "@cloudthinker/subagents/src/workflow/runtime.ts";

import cloudthinker from "@cloudthinker/cloud/src/index.ts";
import { PROVIDER_ID } from "@cloudthinker/cloud/src/provider.ts";
import { CLOUD_ENTRY_TYPE } from "@cloudthinker/cloud/src/runtime.ts";
import { cloudDefaultEnabled } from "@cloudthinker/cloud/src/settings.ts";
import { findLinkedSession } from "@cloudthinker/cloud/src/session.ts";
import { CLOUD_TOOLS } from "@cloudthinker/cloud/src/tools/names.ts";
import { adaptiveSubagentModeGuidance } from "./subagent-modes.ts";
import { formatClock, tasksPane } from "./tasks-pane.ts";

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

export function cloudEnabled(ctx: ExtensionContext): boolean {
	const entry = ctx.sessionManager.getEntries().findLast((item) => item.type === "custom" && item.customType === CLOUD_ENTRY_TYPE);
	if (entry?.type !== "custom") return cloudDefaultEnabled(ctx.cwd, ctx.isProjectTrusted());
	return (entry.data as { enabled?: unknown } | undefined)?.enabled !== false;
}

export function subagentGuidance(modes: readonly { provider: string; id: string }[]): string {
	return `${adaptiveSubagentModeGuidance(modes)} For staged work or a requested workflow, call ct_workflow directly; do not run a pilot Agent first. Use Agent for substantive independent work. Background tasks continue after your turn and resume you on completion; monitoring needs no worker or sleep.`;
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
		sessionManager: options.sessionManager && (options.sessionManager.getSessionFile() || options.sessionManager.getEntries().length > 0)
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

const ADAPTED = Symbol("cloudthinker.adapted");

export function cloudDelegationTool(tool: ToolDefinition): ToolDefinition {
	if ((tool as { [ADAPTED]?: true })[ADAPTED] || !["Agent", "ct_workflow"].includes(tool.name)) return tool;
	const schema = tool.parameters as typeof tool.parameters & { properties?: Record<string, { description?: string; [key: string]: unknown }> };
	const upstreamName = tool.name;
	const properties = { ...schema.properties };
	delete properties.thinking;
	if (upstreamName === "ct_workflow" && properties.args) {
		properties.args = {
			...properties.args,
			type: "object",
			additionalProperties: true,
			description: "Optional object passed to the workflow as global args. Pass JSON values directly, not JSON-encoded strings.",
		};
		const execute = tool.execute.bind(tool);
		tool.execute = async (toolCallId, params, signal, onUpdate, ctx) => {
			const args = (params as { args?: unknown }).args;
			if (args !== undefined && (args === null || typeof args !== "object" || Array.isArray(args))) {
				throw new Error("ct_workflow args must be an object when provided. Pass JSON object fields directly; do not pass a JSON-encoded string.");
			}
			return execute(toolCallId, params, signal, onUpdate, ctx);
		};
	}
	if (properties.model) {
		properties.model = { ...properties.model, description: "CloudThinker agent mode ID, such as cloudthinker/pro. Omit to inherit the parent mode." };
	}
	if (properties.run_in_background) {
		properties.run_in_background = { ...properties.run_in_background, description: "Ignored in interactive sessions, where every agent runs in the background and its completion resumes you. Print and JSON runs always wait for completion." };
	}
	tool.parameters = { ...tool.parameters, properties };
	const replacements: [string | RegExp, string][] = upstreamName === "Agent" ? [
		['- Use model to specify a different model (as "provider/modelId", or fuzzy e.g. "haiku", "sonnet").', '- Use model only to select an advertised CloudThinker agent mode. Omit it to inherit the parent mode.'],
		['- Use thinking to control extended thinking level.\n', ''],
	] : [
		['effort?: string, ', ''],
		[/opts\.effort overrides .*?opts\.isolation:/s, 'opts.isolation:'],
		['agentType, model, effort, isolation', 'agentType, model, isolation'],
		[
			"Concurrent agent() calls are capped at the configured session limit; excess calls queue. Nested workflows share this limit.",
			`Concurrent agent() calls are capped at ${workflowConcurrency()}; excess calls queue. Nested workflows share this limit.`,
		],
	];
	let description = tool.description;
	for (const [pattern, replacement] of replacements) {
		const rewritten = description.replace(pattern, replacement);
		if (rewritten === description) throw new Error(`Unsupported upstream ${tool.name} description; update the CloudThinker adapter.`);
		description = rewritten;
	}
	tool.description = description + "\nIn print and JSON mode, this tool waits for delegated work to complete and returns its results before the CLI exits."
		+ (upstreamName === "Agent" ? "\nIn interactive sessions every agent runs in the background whatever run_in_background says; continue other work until its completion event resumes you." : "");
	if (upstreamName === "Agent") {
		const execute = tool.execute.bind(tool);
		tool.execute = (toolCallId, params, signal, onUpdate, ctx) =>
			execute(toolCallId, ctx.mode === "tui" ? { ...(params as object), run_in_background: true } : params, signal, onUpdate, ctx);
	}
	if (typeof tool.renderResult === "function") {
		const renderResult = tool.renderResult.bind(tool);
		tool.renderResult = (result, options, theme, context) => {
			const component = renderResult(result, options, theme, context);
			const status = (result.details as { status?: string } | undefined)?.status;
			if (upstreamName === "Agent" && status === "background" && !context.isError) return new Text("", 0, 0);
			if (upstreamName !== "ct_workflow" || options.expanded || context.isError) return component;
			return { render: (width: number) => component.render(width).slice(0, 1), invalidate: () => component.invalidate() };
		};
	}
	(tool as { [ADAPTED]?: true })[ADAPTED] = true;
	return tool;
}

export const subagentHost: SubagentHost = {
	resolveModel: resolveCloudMode,
	loaderOptions: childLoaderOptions,
	createSession: createCloudChild,
	modelChoices: (ctx) => ctx.modelRegistry.getAll().filter((model) => model.provider === PROVIDER_ID).map((model) => `${PROVIDER_ID}/${model.id}`),
	taskRows: (rows, active) => tasksPane.setGroup("Agents", rows, active),
	tasks: () => tasksPane.commands.map((task) => ({ id: task.id, label: task.command, stats: formatClock(Date.now() - task.startedAt) })),
	openTask: (id, ui) => tasksPane.open(id, ui),
	stopTask: (id) => tasksPane.commands.find((task) => task.id === id)?.stop() ?? Promise.resolve(),
};

interface NotificationDetails {
	description: string;
	status: string;
	toolUses: number;
	durationMs: number;
	others?: NotificationDetails[];
}

export function notificationLines(details: NotificationDetails, theme: Theme): string {
	return [details, ...(details.others ?? [])].map((item) => {
		const failed = ["error", "stopped", "aborted"].includes(item.status);
		const icon = failed ? theme.fg("error", "✗") : theme.fg("success", "✓");
		const stats = [failed ? item.status : "done", item.toolUses > 0 ? `${item.toolUses} tool use${item.toolUses === 1 ? "" : "s"}` : "", item.durationMs > 0 ? formatClock(item.durationMs) : ""]
			.filter(Boolean)
			.join(" · ");
		return `${icon} ${theme.bold(item.description)} ${theme.fg("dim", stats)}`;
	}).join("\n");
}

export default function bundledSubagents(pi: ExtensionAPI): ReturnType<typeof subagents> {
	setSubagentHost(subagentHost);
	for (const agent of DEFAULT_AGENTS.values()) {
		delete agent.model;
		delete agent.thinking;
	}
	pi.on("before_agent_start", (event, ctx) => ({
		systemPrompt: `${event.systemPrompt}\n\n${subagentGuidance(ctx.modelRegistry.getAll().filter((model) => model.provider === PROVIDER_ID))}`,
	}));
	tasksPane.managed = true;
	return subagents(new Proxy(pi, {
		get(target, key, receiver) {
			if (key === "registerTool") return (tool: ToolDefinition) => target.registerTool(cloudDelegationTool(tool));
			if (key === "registerMessageRenderer") {
				return (customType: string, renderer: MessageRenderer) => target.registerMessageRenderer(customType, customType !== "subagent-notification" ? renderer : (message, options, theme) =>
					options.expanded || !message.details ? renderer(message, options, theme) : new Text(notificationLines(message.details as NotificationDetails, theme), 0, 0));
			}
			return Reflect.get(target, key, receiver);
		},
	}));
}
