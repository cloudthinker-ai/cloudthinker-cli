import type { ExtensionAPI, ExtensionContext, SessionStartEvent } from "@earendil-works/pi-coding-agent";

import {
	CloudThinkerApiError,
	CloudThinkerClient,
	type ConnectionsContext,
	type GatewayModel,
	type Identity,
	type SessionCreated,
	type SessionCredits,
} from "./client.ts";
import { CLOUD_TOOLS } from "./tools/names.ts";
import { BOTH_TAG, LOCAL_TAG, createLegend, sanitizeTerminalText, setMachineState } from "./awareness.ts";
import { PRODUCT_NAME, SessionHeader } from "./header.ts";
import { type HostVersions, readHostVersions } from "./versions.ts";

export const CLOUD_ENTRY_TYPE = "cloudthinker.cloud";
export const CLOUD_OFF_MESSAGE = "Cloud is off for this session, so this tool did not run. It runs commands on the CloudThinker Sandbox, while this session is local-only and the local tools keep running on your machine. Use /cloud on to enable it.";

export const SESSION_ENTRY_TYPE = "cloudthinker";
export const LOCATION_ENTRY_TYPE = "cloudthinker.location";
export const ASK_THREAD_ENTRY_TYPE = "cloudthinker.ask_thread";
export const STATUS_KEY = "cloudthinker";
export const APPROVAL_KEY = "cloudthinker.approval";
export const CREDITS_KEY = "cloudthinker.credits";

export const NOTIFY_HINT = `/${PRODUCT_NAME} notify tells the approvers`;
export const AUTO_MODE_OFF_REASON = "auto_mode_disabled";

export function approvalWidgetLine(
	webUrl: string,
	subject = "Anna",
	hint: string = NOTIFY_HINT,
): string {
	const waiting = `⏸ ${subject} is waiting for your approval in the browser → ${sanitizeTerminalText(webUrl)}`;
	return hint ? `${waiting} · ${hint}` : waiting;
}

export interface AutoMode {
	enabled: boolean;
	canEdit: boolean;
}

export function autoModeFrom(session: SessionCreated): AutoMode {
	return { enabled: session.auto_mode.enabled, canEdit: session.auto_mode.can_edit };
}

export interface AskThread {
	conversation_id: string;
	web_url: string;
}

export interface MemorySnapshot {
	memoryIndex: string;
	userNotes: string;
}

export class CloudThinkerRuntime {
	cloudEnabled = true;
	private disabledCloudTools: string[] = [];
	readonly client: CloudThinkerClient;
	session: SessionCreated | undefined;
	identity: Identity | undefined;
	models: GatewayModel[] = [];
	connections: ConnectionsContext = { xml: "", prefixes: [] };
	autoMode: AutoMode | undefined;
	askThread: AskThread | undefined;
	memory: MemorySnapshot | undefined;
	credits: SessionCredits | undefined;
	approvalRunId: string | undefined;

	readonly pi: ExtensionAPI;
	readonly header: SessionHeader;
	readonly versions: HostVersions;
	readonly root: boolean;
	readonly legend = createLegend();
	startEvent: SessionStartEvent | undefined;
	sourceConversationId: string | undefined;
	afterLink: ((ctx: ExtensionContext) => Promise<void>) | undefined;

	private context: ExtensionContext | undefined;

	constructor(
		pi: ExtensionAPI,
		client: CloudThinkerClient = new CloudThinkerClient(),
		versions: HostVersions = readHostVersions(),
		root = false,
	) {
		this.pi = pi;
		this.client = client;
		this.versions = versions;
		this.header = new SessionHeader(versions);
		this.root = root;
	}

	bind(context: ExtensionContext): void {
		this.context = context;
	}

	setCloudEnabled(enabled: boolean, persist = true): void {
		const active = this.pi.getActiveTools();
		if (!enabled) {
			this.disabledCloudTools = [...new Set([
				...this.disabledCloudTools,
				...active.filter((name) => CLOUD_TOOLS.includes(name)),
			])];
			this.pi.setActiveTools(active.filter((name) => !CLOUD_TOOLS.includes(name)));
		} else {
			this.pi.setActiveTools([...new Set([...active, ...this.disabledCloudTools])]);
			this.disabledCloudTools = [];
		}
		this.cloudEnabled = enabled;
		this.context?.ui.setStatus(CLOUD_ENTRY_TYPE, enabled ? BOTH_TAG : LOCAL_TAG);
		if (this.root) setMachineState({ cloudEnabled: enabled, ...(this.context?.cwd ? { cwd: this.context.cwd } : {}) });
		if (persist) this.pi.appendEntry(CLOUD_ENTRY_TYPE, { enabled });
	}

	get connectedPrefixes(): string[] {
		return this.connections.prefixes;
	}

	setAutoMode(next: AutoMode): void {
		const previous = this.autoMode;
		this.autoMode = next;
		this.header.set({ autoMode: next.enabled });
		const changed =
			previous !== undefined &&
			(previous.enabled !== next.enabled || previous.canEdit !== next.canEdit);
		if (!changed || !this.session) return;
		this.session = {
			...this.session,
			auto_mode: { enabled: next.enabled, can_edit: next.canEdit },
		};
		this.pi.appendEntry(SESSION_ENTRY_TYPE, this.session);
	}

	noteWriteVerdict(verdictReason: string): void {
		if (!this.autoMode) return;
		const enabled = verdictReason !== AUTO_MODE_OFF_REASON;
		if (enabled !== this.autoMode.enabled) {
			this.setAutoMode({ ...this.autoMode, enabled });
		}
	}

	setStatus(text: string | undefined): void {
		this.context?.ui.setStatus(STATUS_KEY, text);
	}

	setCredits(text: string | undefined): void {
		this.context?.ui.setStatus(CREDITS_KEY, text);
	}

	awaitApproval(runId: string, webUrl: string, subject?: string, hint?: string): void {
		this.approvalRunId = runId;
		this.context?.ui.setWidget(
			APPROVAL_KEY,
			[approvalWidgetLine(webUrl, subject, hint)],
			{ placement: "aboveEditor" },
		);
	}

	clearApproval(): void {
		this.approvalRunId = undefined;
		this.context?.ui.setWidget(APPROVAL_KEY, undefined);
	}

	setTitle(title: string): void {
		this.context?.ui.setTitle(title);
	}

	notify(message: string, type: "info" | "warning" | "error" = "info"): void {
		this.context?.ui.notify(message, type);
	}

	requireSession(): SessionCreated {
		if (!this.cloudEnabled) throw new CloudThinkerApiError(0, CLOUD_OFF_MESSAGE);
		if (!this.session) {
			throw new CloudThinkerApiError(
				0,
				"This session is not linked to CloudThinker yet, so cloud tools are unavailable. Check the network and restart.",
			);
		}
		return this.session;
	}

	reset(): void {
		this.session = undefined;
		this.autoMode = undefined;
		this.askThread = undefined;
		this.memory = undefined;
		this.credits = undefined;
		this.legend.reset();
		this.setStatus(undefined);
		this.setCredits(undefined);
		this.clearApproval();
		this.header.reset();
	}
}

export function describeError(error: unknown): string {
	if (error instanceof CloudThinkerApiError) return error.message;
	return error instanceof Error ? error.message : String(error);
}

export function detach(work: () => Promise<void>, onError: (error: unknown) => void): void {
	void work().catch(onError);
}
