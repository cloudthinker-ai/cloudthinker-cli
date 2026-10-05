import { PROVIDER_ID } from "@cloudthinker/cloud/src/provider.ts";

import { InteractiveMode } from "@earendil-works/pi-coding-agent";

import { formatNoApiKeyFoundMessage, formatNoModelSelectedMessage, formatNoModelsAvailableMessage } from "../node_modules/@earendil-works/pi-coding-agent/dist/core/auth-guidance.js";
import { AgentSession } from "../node_modules/@earendil-works/pi-coding-agent/dist/core/agent-session.js";
import { ModelRuntime } from "../node_modules/@earendil-works/pi-coding-agent/dist/core/model-runtime.js";

export const MODEL_SCOPE = `${PROVIDER_ID}/*`;

export const NO_CLOUD_MODEL = "No CloudThinker Agent mode is loaded, so the message was not sent. Run /cloud retry, then press ↑ to send it again.";

export function modelScopeArgs(argv: string[]): string[] {
	return argv.includes("--models") ? [] : ["--models", MODEL_SCOPE];
}

interface RuntimeModel {
	provider: string;
}

interface ModelAvailability {
	getAvailableSnapshot(this: unknown): readonly RuntimeModel[];
	getAvailable(this: unknown, ...args: unknown[]): Promise<readonly RuntimeModel[]>;
	hasConfiguredAuth(this: unknown, providerId: string): boolean;
}

function cloudOnly<T extends RuntimeModel>(models: readonly T[]): T[] {
	return models.filter((model) => model.provider === PROVIDER_ID);
}

export function applyCloudOnlyModels(): void {
	const runtime = ModelRuntime.prototype as unknown as ModelAvailability;
	const { getAvailableSnapshot, getAvailable, hasConfiguredAuth } = runtime;
	if (typeof getAvailableSnapshot !== "function" || typeof getAvailable !== "function" || typeof hasConfiguredAuth !== "function") {
		throw new Error("pi's ModelRuntime availability seam changed, so a vendor model could be picked");
	}
	runtime.getAvailableSnapshot = function () {
		return cloudOnly(getAvailableSnapshot.call(this));
	};
	runtime.getAvailable = async function (...args) {
		return cloudOnly(await getAvailable.apply(this, args));
	};
	runtime.hasConfiguredAuth = function (providerId) {
		return providerId === PROVIDER_ID && hasConfiguredAuth.call(this, providerId);
	};
}

interface PromptingSession {
	model?: RuntimeModel;
	prompt(this: PromptingSession, ...args: unknown[]): Promise<void>;
}

interface StartingMode {
	options: { modelFallbackMessage?: string };
	run(this: StartingMode): Promise<void>;
}

export function cloudModelError(error: unknown, model: RuntimeModel | undefined): unknown {
	if (!(error instanceof Error) || model?.provider === PROVIDER_ID) return error;
	const provider = model?.provider ?? "unknown";
	const vendor = [formatNoModelSelectedMessage(), formatNoApiKeyFoundMessage(provider)];
	if (vendor.includes(error.message) || error.message.startsWith(`Authentication failed for "${provider}".`)) return new Error(NO_CLOUD_MODEL);
	return error;
}

export function applyCloudModelGuidance(): void {
	const session = AgentSession.prototype as unknown as PromptingSession;
	const mode = InteractiveMode.prototype as unknown as StartingMode;
	const { prompt } = session;
	const { run } = mode;
	if (typeof prompt !== "function" || typeof run !== "function") {
		throw new Error("pi's prompt or startup seam changed, so a missing agent mode would point the user at vendor login");
	}
	session.prompt = async function (...args) {
		try {
			await prompt.apply(this, args);
		} catch (error) {
			throw cloudModelError(error, this.model);
		}
	};
	mode.run = function () {
		if (this.options.modelFallbackMessage === formatNoModelsAvailableMessage()) this.options.modelFallbackMessage = undefined;
		return run.call(this);
	};
}
