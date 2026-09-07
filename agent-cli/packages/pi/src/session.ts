import type {
	ExtensionContext,
	SessionEntry,
	SessionStartEvent,
} from "@earendil-works/pi-coding-agent";

import type { SessionCreated } from "./client.ts";
import {
	ASK_THREAD_ENTRY_TYPE,
	type AskThread,
	type CloudThinkerRuntime,
	SESSION_ENTRY_TYPE,
	autoModeFrom,
	describeError,
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

export async function linkSession(
	runtime: CloudThinkerRuntime,
	event: SessionStartEvent,
	ctx: ExtensionContext,
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
		source_conversation_id: forked ? carried?.conversation_id : undefined,
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

export async function startSession(
	runtime: CloudThinkerRuntime,
	event: SessionStartEvent,
	ctx: ExtensionContext,
): Promise<void> {
	const [session, identity] = await Promise.allSettled([
		linkSession(runtime, event, ctx),
		runtime.client.whoami(),
		refreshConnections(runtime),
	]);
	if (identity.status === "fulfilled") runtime.identity = identity.value;
	runtime.header.set({
		link: session.status === "fulfilled" ? "linked" : "unavailable",
		workspaceName: runtime.identity?.workspace_name,
		userEmail: runtime.identity?.user_email,
		webUrl: runtime.session?.web_url,
		autoMode: runtime.autoMode?.enabled,
	});
	if (session.status === "rejected") {
		runtime.setStatus("✕ cloud unavailable");
		runtime.notify(
			`CloudThinker session could not be opened, so cloud tools and the model gateway are unavailable: ${describeError(session.reason)}`,
			"error",
		);
	}
}
