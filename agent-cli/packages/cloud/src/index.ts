import { randomUUID } from "node:crypto";
import { basename } from "node:path";

import { SettingsManager, getAgentDir } from "@earendil-works/pi-coding-agent";
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";

import { withinBudget } from "./async.ts";
import { LOCAL_TOOLS, MISSING_LOCAL_FILE_HINT, localMissingFile, sanitizeTerminalText, setMachineState } from "./awareness.ts";
import { registerCommands } from "./commands.ts";
import { registerContinuation } from "./continuation.ts";
import { CreditsMeter } from "./credits.ts";
import { PRODUCT_NAME } from "./header.ts";
import { LocationTracker } from "./location.ts";
import { fetchMemory } from "./memory.ts";
import { SessionMirror, formatMirrorStatus } from "./mirror.ts";
import { appendPromptBlock, buildPromptBlock } from "./prompt.ts";
import {
	MODELS_UNAVAILABLE_STATUS,
	NO_MODES_REASON,
	PROVIDER_ID,
	applyConversationHeader,
	gatewayErrorMessage,
	modelsUnavailableMessage,
	pinProviderWorkspace,
	registerProvider,
} from "./provider.ts";
import { CLOUD_ENTRY_TYPE, CloudThinkerRuntime, describeError, detach } from "./runtime.ts";
import { cloudDefaultEnabled, resolveCloudEnabled } from "./settings.ts";
import { linkLazily, refreshConnections, refreshIdentity, restoreModels, startLocalReviewSession, startSession } from "./session.ts";
import { discoverSkillPaths, hasSkillIndex, refreshSkills } from "./skills.ts";
import { statusLineFooter } from "./status-line.ts";
import { markStartup } from "./timing.ts";
import { bundledDoctorPath, bundledReviewSetupPath, bundledTourPath } from "./onboarding.ts";
import { registerCommandTools } from "./tools/command-tools.ts";
import { registerAsk } from "./tools/ct-ask.ts";
import { registerSandboxRead } from "./tools/ct-sandbox-read.ts";
import { registerSandboxWrite } from "./tools/ct-sandbox-write.ts";
import { registerLocalApproval } from "./local-approval.ts";
import { registerRunStatus } from "./tools/ct-run-status.ts";
import { registerConfig } from "./tools/ct-config.ts";
import { registerDoctor } from "./doctor.ts";
import { registerReadTaskOutput } from "./tools/read-task-output.ts";
import { registerLocalReviewBoundary } from "./tools/local-review-boundary.ts";
import { CONNECTION_MENTIONS_ENTRY, mentionContext, resolveConnectionMentions, setConnectionSource } from "./connection-mentions.ts";
import { reducedMotion, SERVER_TICK_MS } from "./motion.ts";

const SHUTDOWN_FLUSH_MS = 5_000;
export const LINKING_STATUS = "linking cloud…";

export function sessionTitle(cwd: string, workspaceName: string | undefined): string {
	const parts = [PRODUCT_NAME, `${sanitizeTerminalText(basename(cwd))} (local)`];
	if (workspaceName) parts.push(`${sanitizeTerminalText(workspaceName)} (cloud)`);
	return parts.join(" · ");
}

export interface CloudThinkerSessionOptions {
	cloudEnabled?: boolean;
	localReview?: boolean;
	sourceConversationId?: string;
	selectedAgentReference?: string;
	changelogPath?: string;
}

export default async function cloudthinker(pi: ExtensionAPI, options: CloudThinkerSessionOptions = {}): Promise<void> {
	const root = options.cloudEnabled === undefined && options.sourceConversationId === undefined;
	const runtime = new CloudThinkerRuntime(
		pi,
		undefined,
		undefined,
		root,
		options.selectedAgentReference,
	);
	if (root) setConnectionSource(() => runtime.cloudEnabled ? runtime.connections.connections ?? [] : []);
	const localReview = options.localReview === true;
	const mirror = new SessionMirror(runtime.client, (status) =>
		runtime.setStatus(formatMirrorStatus(status)),
	);
	let sessionGeneration = 0;
	let initialStartupSeen = false;
	const locations = new LocationTracker(pi);
	const credits = new CreditsMeter(runtime);

	const warn = (label: string) => (error: unknown) => {
		runtime.notify(`CloudThinker ${label}: ${describeError(error)}`, "warning");
	};
	const silent = (): void => {};

	const modelsUnavailable = await registerProvider(runtime);
	markStartup("agent.models");
	let modelsUnavailableAnnounced = false;
	if (!localReview) {
		registerSandboxRead(runtime);
		registerSandboxWrite(runtime);
		registerLocalApproval(pi, () => runtime.autoMode);
		registerReadTaskOutput(runtime);
		registerAsk(runtime);
		registerRunStatus(runtime);
		registerConfig(pi);
		registerDoctor(pi);
		registerCommands(runtime, { changelogPath: options.changelogPath });
	}
	registerContinuation(pi);

	const workspaceId = (): string | undefined =>
		runtime.identity?.workspace_id ?? runtime.session?.workspace_id;

	const sync = (ctx: ExtensionContext): void => {
		runtime.bind(ctx);
		mirror.sync(ctx.sessionManager.getEntries(), silent);
	};

	const initializeLinked = async (
		ctx: ExtensionContext,
		generation: number = sessionGeneration,
	): Promise<void> => {
		const session = runtime.session;
		if (!session) return;
		pinProviderWorkspace(runtime, session.workspace_id);
		detach(async () => {
			await mirror.link(session.conversation_id).catch(silent);
			if (
				generation !== sessionGeneration ||
				runtime.session?.conversation_id !== session.conversation_id
			) return;
			sync(ctx);
			credits.refresh();
		}, silent);
		detach(() => locations.record(ctx.cwd), silent);
		detach(async () => { await registerCommandTools(runtime); }, silent);
		detach(async () => {
			runtime.memory = await fetchMemory(runtime.client, session.conversation_id);
		}, silent);
		const skillsWorkspace = workspaceId();
		if (skillsWorkspace) {
			const refresh = async (): Promise<void> => {
				await refreshSkills(runtime.client, skillsWorkspace, undefined, (name, error) => {
					warn(`skill "${name}"`)(error);
				});
			};
			if (await hasSkillIndex(skillsWorkspace)) {
				detach(refresh, warn("skills"));
			} else {
				await refresh().catch(warn("skills"));
			}
		}
	};
	runtime.afterLink = initializeLinked;

	pi.on("session_start", async (event, ctx) => {
		const generation = ++sessionGeneration;
		runtime.bind(ctx);
		runtime.startEvent = event;
		runtime.sourceConversationId = options.sourceConversationId;
		mirror.unlink();
		runtime.reset();
		const entries = ctx.sessionManager.getEntries();
		const hasUserMessage = entries.some((entry) => entry.type === "message" && entry.message.role === "user");
		const showLaunchPresentation = event.reason === "startup" && !initialStartupSeen && !hasUserMessage;
		initialStartupSeen = true;
		runtime.header.setPresentation(showLaunchPresentation ? "launch" : "session");
		const cloudEntry = entries.findLast(
			(entry) => entry.type === "custom" && entry.customType === CLOUD_ENTRY_TYPE,
		);
		const cloudData = cloudEntry?.type === "custom" ? cloudEntry.data as { enabled?: unknown } | undefined : undefined;
		runtime.setCloudEnabled(localReview ? false : resolveCloudEnabled(cloudData?.enabled, options.cloudEnabled, cloudDefaultEnabled(ctx.cwd, ctx.isProjectTrusted())), false);
		if (options.cloudEnabled === false && cloudData?.enabled !== false) pi.appendEntry(CLOUD_ENTRY_TYPE, { enabled: false });
		locations.reset();
		if (ctx.mode === "tui") {
			if (reducedMotion()) ctx.ui.setWorkingIndicator({ intervalMs: SERVER_TICK_MS });
			if (root && !localReview) ctx.ui.setFooter(statusLineFooter(runtime, ctx));
			runtime.setTitle(sessionTitle(ctx.cwd, undefined));
			if (SettingsManager.create(ctx.cwd, getAgentDir()).getQuietStartup() !== true) {
				runtime.header.listen((handler) => ctx.ui.onTerminalInput(handler));
				ctx.ui.setHeader(runtime.header.factory);
			}
		}
		const link = async (): Promise<void> => {
			if (localReview) {
				try {
					await startLocalReviewSession(runtime);
				} catch (error) {
					runtime.setStatus("cloud unavailable");
					runtime.notify(`CloudThinker review session could not be opened: ${describeError(error)}`, "error");
				}
			} else if (runtime.cloudEnabled) {
				await startSession(runtime, event, ctx, options.sourceConversationId);
			}
			if (runtime.closed) return;
			if (!runtime.linkFailure) runtime.setStatus(undefined);
			const announceMissingModels = (): void => {
				if (runtime.models.length > 0 || runtime.linkFailure) return;
				runtime.setStatus(MODELS_UNAVAILABLE_STATUS);
				if (modelsUnavailableAnnounced) return;
				modelsUnavailableAnnounced = true;
				runtime.notify(modelsUnavailableMessage(modelsUnavailable ?? NO_MODES_REASON), "error");
		};
		if (runtime.session && runtime.models.length === 0) detach(async () => { await restoreModels(runtime, ctx); announceMissingModels(); }, silent);
		else announceMissingModels();
		markStartup("agent.session_link");
		if (root) setMachineState({
			cwd: ctx.cwd,
			linked: runtime.session !== undefined,
			workspaceName: runtime.identity?.workspace_name,
			connectionCount: runtime.connectedPrefixes.length,
			cloudEnabled: runtime.cloudEnabled,
		});
		if (ctx.mode === "tui") {
			runtime.setTitle(sessionTitle(ctx.cwd, runtime.identity?.workspace_name));
		}
		const session = runtime.session;
		if (!session) return;
		if (!localReview) await initializeLinked(ctx, generation);
		markStartup("agent.session_start");
		};
		if (ctx.mode === "tui" && event.reason !== "startup" && !localReview) {
			if (runtime.cloudEnabled) runtime.setStatus(LINKING_STATUS);
			detach(link, (error) => {
				if (!runtime.closed) warn("session")(error);
			});
			return;
		}
		await link();
	});

	pi.on("resources_discover", async () => ({
		skillPaths: localReview ? [] : [bundledTourPath(), bundledDoctorPath(), bundledReviewSetupPath(), ...await discoverSkillPaths(workspaceId())],
	}));

	pi.on("tool_result", (event) => {
		if (!event.isError || !LOCAL_TOOLS.includes(event.toolName)) return;
		const body = event.content.map((block) => (block.type === "text" ? block.text : "")).join("\n");
		if (!localMissingFile(body)) return;
		return { content: [...event.content, { type: "text" as const, text: MISSING_LOCAL_FILE_HINT }] };
	});
	if (localReview) registerLocalReviewBoundary(pi);

	pi.on("message_end", (event) => {
		const message = event.message;
		if (message.role !== "assistant" || message.provider !== PROVIDER_ID || message.stopReason !== "error") return;
		const readable = gatewayErrorMessage(message.errorMessage);
		if (!readable) return;
		return { message: { ...message, errorMessage: readable } };
	});

	pi.on("before_provider_headers", (event, ctx) => {
		runtime.bind(ctx);
		applyConversationHeader(
			event.headers,
			ctx.model?.provider,
			runtime.session?.conversation_id,
			runtime.turnId,
			localReview ? "review" : undefined,
		);
	});

	pi.on("before_agent_start", async (event, ctx) => {
		runtime.bind(ctx);
		runtime.header.setPresentation("session");
		runtime.turnId = randomUUID();
		if (localReview) {
			if (!runtime.session) throw new Error("CloudThinker local review session is unavailable");
			return { systemPrompt: event.systemPrompt };
		}
		if (!runtime.session) {
			await linkLazily(runtime, ctx);
			detach(() => refreshIdentity(runtime), silent);
			detach(() => runtime.afterLink?.(ctx) ?? Promise.resolve(), silent);
		} else if (runtime.cloudEnabled) {
			detach(() => refreshConnections(runtime), silent);
		}
		const mentions = runtime.cloudEnabled && typeof event.prompt === "string" ? resolveConnectionMentions(event.prompt, runtime.connections.connections ?? []) : [];
		return {
			systemPrompt: appendPromptBlock(event.systemPrompt, buildPromptBlock(runtime)),
			...(mentions.length > 0
				? { message: { customType: CONNECTION_MENTIONS_ENTRY, content: [{ type: "text" as const, text: mentionContext(mentions) }], display: false, details: { mentions } } }
				: {}),
		};
	});

	if (!localReview) pi.on("turn_end", (_event, ctx) => {
		sync(ctx);
		detach(() => locations.record(ctx.cwd), silent);
	});
	pi.on("agent_end", (_event, ctx) => {
		runtime.turnId = undefined;
		if (localReview) return;
		sync(ctx);
		credits.refresh();
	});
	if (!localReview) {
		pi.on("session_compact", (_event, ctx) => sync(ctx));
		pi.on("session_tree", (_event, ctx) => sync(ctx));
	}

	pi.on("session_shutdown", async () => {
		runtime.header.stop();
		runtime.closed = true;
		runtime.cancelRetry();
	});
	if (!localReview) pi.on("session_shutdown", async (_event, ctx) => {
		runtime.bind(ctx);
		await withinBudget(
			mirror.flush(ctx.sessionManager.getEntries()).catch(silent),
			SHUTDOWN_FLUSH_MS,
		);
	});
}
