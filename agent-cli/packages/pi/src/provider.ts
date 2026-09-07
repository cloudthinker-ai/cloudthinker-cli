import type { ProviderModelConfig } from "@earendil-works/pi-coding-agent";

import { type GatewayModel, TOKEN_COMMAND } from "./client.ts";
import { type CloudThinkerRuntime, describeError } from "./runtime.ts";
import { isUuid } from "./uuid.ts";

export const PROVIDER_ID = "cloudthinker";
export const DEFAULT_MODE = "pro";
export const DEFAULT_MODEL = `${PROVIDER_ID}/${DEFAULT_MODE}`;
export const CONVERSATION_HEADER = "X-CloudThinker-Conversation";

export const MODELS_UNAVAILABLE_STATUS = "✕ cloud models unavailable";

export function modelsUnavailableMessage(reason: string): string {
	return `CloudThinker could not list its agent modes, so no cloud model is available until you restart: ${reason}`;
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
	return pinned && isUuid(pinned) ? `!${TOKEN_COMMAND} --workspace ${pinned}` : `!${TOKEN_COMMAND}`;
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

export async function registerProvider(
	runtime: CloudThinkerRuntime,
): Promise<string | undefined> {
	try {
		runtime.models = orderModes(await runtime.client.listModels());
	} catch (error) {
		return describeError(error);
	}
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
): void {
	if (provider !== PROVIDER_ID) return;
	if (!conversationId) return;
	headers[CONVERSATION_HEADER] = conversationId;
}
