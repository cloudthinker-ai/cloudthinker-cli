import type {
	ExtensionContext,
	SessionEntry,
	SessionStartEvent,
} from "@earendil-works/pi-coding-agent";

import { setMachineState } from "./awareness.ts";
import type { SessionCreated } from "./client.ts";
import { classifyCloudError, failureNotice, failureStatus, retryDelay } from "./cloud-error.ts";
import { DEFAULT_MODE, PROVIDER_ID, pinProviderWorkspace, registerProvider } from "./provider.ts";
import {
	ASK_THREAD_ENTRY_TYPE,
	type AskThread,
	type CloudThinkerRuntime,
	SESSION_ENTRY_TYPE,
	autoModeFrom,
} from "./runtime.ts";

function lastCustomData<T>(entries: SessionEntry[], customType: string): T | undefined {
	for (let index = entries.length - 1; index >= 0; index -= 1) {
		const entry = entries[index];
		if (entry?.type === "custom" && entry.customType === customType) {
			return entry.data as T;
		}
	}
	return undefined;
}

export function findLinkedSession(entries: SessionEntry[]): SessionCreated | undefined {
	const data = lastCustomData<SessionCreated>(entries, SESSION_ENTRY_TYPE);
	return data && typeof data.conversation_id === "string" ? data : undefined;
}

export function findAskThread(entries: SessionEntry[]): AskThread | undefined {
	const data = lastCustomData<AskThread>(entries, ASK_THREAD_ENTRY_TYPE);
	return data && typeof data.conversation_id === "string" ? data : undefined;
}

export function linkSession(
	runtime: CloudThinkerRuntime,
	event: SessionStartEvent,
	ctx: ExtensionContext,
	sourceConversationId?: string,
): Promise<SessionCreated> {
	if (runtime.linking) return runtime.linking;
	const linking = openSession(runtime, event, ctx, sourceConversationId).finally(() => {
		if (runtime.linking === linking) runtime.linking = undefined;
	});
	runtime.linking = linking;
	return linking;
}

async function openSession(
	runtime: CloudThinkerRuntime,
	event: SessionStartEvent,
	ctx: ExtensionContext,
	sourceConversationId?: string,
): Promise<SessionCreated> {
	const entries = ctx.sessionManager.getEntries();
	const carried = findLinkedSession(entries);
	const forked = event.reason === "fork";
	if (carried && !forked) {
		runtime.session = carried;
		runtime.setAutoMode(autoModeFrom(carried));
		runtime.askThread = findAskThread(entries);
		return carried;
	}
	const created = await runtime.client.createSession({
		cwd: ctx.cwd,
		source_conversation_id: forked ? carried?.conversation_id : sourceConversationId,
	});
	runtime.session = created;
	runtime.setAutoMode(autoModeFrom(created));
	runtime.askThread = undefined;
	runtime.pi.appendEntry(SESSION_ENTRY_TYPE, created);
	return created;
}

export async function refreshConnections(runtime: CloudThinkerRuntime): Promise<void> {
	try {
		runtime.connections = await runtime.client.getConnectionsContext();
	} catch {
		return;
	}
}

export function newSessionEvent(): SessionStartEvent {
	return { type: "session_start", reason: "new" } as SessionStartEvent;
}

function applySessionHeader(runtime: CloudThinkerRuntime, link: "linked" | "unavailable"): void {
	runtime.header.set({
		link,
		workspaceName: runtime.identity?.workspace_name,
		userEmail: runtime.identity?.user_email,
		webUrl: runtime.session?.web_url,
		autoMode: runtime.autoMode?.enabled,
	});
}

export async function refreshIdentity(runtime: CloudThinkerRuntime): Promise<void> {
	const [identity] = await Promise.allSettled([
		runtime.client.whoami(),
		refreshConnections(runtime),
	]);
	if (identity.status === "fulfilled") runtime.identity = identity.value;
	applySessionHeader(runtime, runtime.session ? "linked" : "unavailable");
}

export async function startSession(
	runtime: CloudThinkerRuntime,
	event: SessionStartEvent,
	ctx: ExtensionContext,
	sourceConversationId?: string,
): Promise<void> {
	const [session, identity] = await Promise.allSettled([
		linkSession(runtime, event, ctx, sourceConversationId),
		runtime.client.whoami(),
		refreshConnections(runtime),
	]);
	if (identity.status === "fulfilled") runtime.identity = identity.value;
	applySessionHeader(runtime, session.status === "fulfilled" ? "linked" : "unavailable");
	if (session.status === "rejected") reportLinkFailure(runtime, session.reason, ctx);
	else clearLinkFailure(runtime);
}

export function reportLinkFailure(runtime: CloudThinkerRuntime, error: unknown, ctx: ExtensionContext): void {
	const failure = classifyCloudError(error);
	const repeated = runtime.linkFailure?.kind === failure.kind;
	runtime.linkFailure = failure;
	const delay = failure.retry ? retryDelay(runtime.linkAttempts) : undefined;
	runtime.setStatus(failureStatus(failure, delay));
	if (!repeated) runtime.notify(failureNotice(failure), "error");
	if (delay !== undefined) runtime.scheduleRetry(delay, () => void retryLink(runtime, ctx).catch(() => undefined));
}

function clearLinkFailure(runtime: CloudThinkerRuntime): void {
	const recovered = runtime.linkFailure !== undefined;
	runtime.cancelRetry();
	runtime.linkFailure = undefined;
	runtime.linkAttempts = 0;
	if (!recovered) return;
	runtime.setStatus(undefined);
	runtime.notify("CloudThinker cloud is back: cloud tools and the model gateway are available.", "info");
}

export async function restoreModels(runtime: CloudThinkerRuntime, ctx: ExtensionContext): Promise<boolean> {
	if (runtime.models.length > 0 || await registerProvider(runtime)) return false;
	const model = ctx.modelRegistry.find(PROVIDER_ID, DEFAULT_MODE) ?? ctx.modelRegistry.find(PROVIDER_ID, runtime.models[0]!.id);
	if (model && ctx.model?.provider !== PROVIDER_ID) await runtime.pi.setModel(model);
	return true;
}

export async function retryLink(runtime: CloudThinkerRuntime, ctx: ExtensionContext): Promise<void> {
	runtime.cancelRetry();
	if (runtime.closed || !runtime.cloudEnabled || runtime.session) return;
	runtime.linkAttempts += 1;
	runtime.bind(ctx);
	await startSession(runtime, runtime.startEvent ?? newSessionEvent(), ctx, runtime.sourceConversationId);
	if (!runtime.session) return;
	await restoreModels(runtime, ctx);
	if (runtime.root) {
		setMachineState({ linked: true, workspaceName: runtime.identity?.workspace_name, connectionCount: runtime.connectedPrefixes.length });
	}
	await runtime.afterLink?.(ctx);
}

export async function startLocalReviewSession(
	runtime: CloudThinkerRuntime,
): Promise<SessionCreated> {
	const created = await runtime.client.createSession({
		cwd: "local-review",
		title: "Local code review",
		skip_sandbox_warmup: true,
	});
	runtime.session = created;
	runtime.setAutoMode(autoModeFrom(created));
	pinProviderWorkspace(runtime, created.workspace_id);
	runtime.pi.appendEntry(SESSION_ENTRY_TYPE, created);
	return created;
}

export async function linkLazily(
	runtime: CloudThinkerRuntime,
	ctx: ExtensionContext,
): Promise<void> {
	if (runtime.session) return;
	try {
		await linkSession(
			runtime,
			runtime.startEvent ?? newSessionEvent(),
			ctx,
			runtime.sourceConversationId,
		);
		applySessionHeader(runtime, "linked");
		clearLinkFailure(runtime);
	} catch (error) {
		reportLinkFailure(runtime, error, ctx);
	}
}
