import type { ProviderModelConfig } from "@earendil-works/pi-coding-agent";

import { type GatewayModel, tokenCommand } from "./client.ts";
import { type CloudThinkerRuntime, describeError, detach } from "./runtime.ts";
import { isUuid } from "./uuid.ts";

export const PROVIDER_ID = "cloudthinker";
export const DEFAULT_MODE = "pro";
export const DEFAULT_MODEL = `${PROVIDER_ID}/${DEFAULT_MODE}`;
export const CONVERSATION_HEADER = "X-CloudThinker-Conversation";
export const TURN_HEADER = "X-CloudThinker-Turn";

export const MODELS_UNAVAILABLE_STATUS = "no agent modes: /cloud retry";
export const NO_MODES_REASON = "the server advertised no agent mode";

export function modelsUnavailableMessage(reason: string): string {
	return `CloudThinker could not list its agent modes, so no cloud model is available yet; /cloud retry loads them: ${reason}`;
}

export function orderModes(models: GatewayModel[]): GatewayModel[] {
	return [...models].sort((left, right) => {
		if (left.id === right.id) return 0;
		if (left.id === DEFAULT_MODE) return -1;
		if (right.id === DEFAULT_MODE) return 1;
		return left.id.localeCompare(right.id);
	});
}

export function apiKeySpec(
	workspaceId?: string,
	env: NodeJS.ProcessEnv = process.env,
): string {
	if (env.CLOUDTHINKER_TOKEN?.trim()) return "$CLOUDTHINKER_TOKEN";
	const pinned = workspaceId ?? env.CLOUDTHINKER_WORKSPACE?.trim();
	const command = tokenCommand(env);
	return pinned && isUuid(pinned) ? `!${command} --workspace ${pinned}` : `!${command}`;
}

export const NO_PRICE = { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 } as const;

export function priced(models: GatewayModel[]): ProviderModelConfig[] {
	return models.map((model) => ({ ...model, cost: { ...NO_PRICE } }));
}

function register(runtime: CloudThinkerRuntime, workspaceId?: string): void {
	runtime.pi.registerProvider(PROVIDER_ID, {
		name: "CloudThinker",
		baseUrl: `${runtime.client.apiUrl}/agent-cli/llm`,
		apiKey: apiKeySpec(workspaceId),
		authHeader: true,
		api: "anthropic-messages",
		models: priced(runtime.models),
	});
}

const knownModes = new Map<string, GatewayModel[]>();

async function refreshKnownModes(runtime: CloudThinkerRuntime): Promise<void> {
	const models = orderModes(await runtime.client.listModels());
	if (models.length === 0) {
		knownModes.delete(runtime.client.apiUrl);
		return;
	}
	knownModes.set(runtime.client.apiUrl, models);
	if (runtime.closed || JSON.stringify(models) === JSON.stringify(runtime.models)) return;
	runtime.models = models;
	register(runtime, runtime.session?.workspace_id);
}

export async function registerProvider(
	runtime: CloudThinkerRuntime,
): Promise<string | undefined> {
	const known = knownModes.get(runtime.client.apiUrl);
	if (known) {
		runtime.models = known;
		register(runtime);
		detach(() => refreshKnownModes(runtime), () => {});
		return undefined;
	}
	try {
		runtime.models = orderModes(await runtime.client.listModels());
	} catch (error) {
		return describeError(error);
	}
	if (runtime.models.length === 0) return NO_MODES_REASON;
	knownModes.set(runtime.client.apiUrl, runtime.models);
	register(runtime);
	return undefined;
}

export function pinProviderWorkspace(
	runtime: CloudThinkerRuntime,
	workspaceId: string,
): void {
	if (runtime.models.length === 0) return;
	register(runtime, workspaceId);
}

export function applyConversationHeader(
	headers: Record<string, string | null>,
	provider: string | undefined,
	conversationId: string | undefined,
	turnId?: string,
): void {
	if (provider !== PROVIDER_ID) return;
	if (!conversationId) return;
	headers[CONVERSATION_HEADER] = conversationId;
	if (turnId) headers[TURN_HEADER] = turnId;
}
