import { mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { dirname } from "node:path";

import type { SessionEntry } from "@earendil-works/pi-coding-agent";

import { singleFlight } from "./async.ts";
import { CloudThinkerApiError } from "./client.ts";
import type { CloudThinkerClient, MirrorEntryInput } from "./client.ts";
import { outboxPath, rejectedPath } from "./paths.ts";

export const MAX_ENTRIES_PER_BATCH = 200;
export const MAX_PAYLOAD_BYTES = 1024 * 1024;
const MAX_BACKOFF_MS = 60_000;
const FIRST_BACKOFF_MS = 1_000;
const ENTRY_PAGE_LIMIT = 500;

export function toMirrorEntry(entry: SessionEntry): MirrorEntryInput {
	return {
		entry_id: entry.id,
		parent_id: entry.parentId ?? null,
		entry_type: entry.type,
		payload: entry,
	};
}

export function payloadBytes(entry: MirrorEntryInput): number {
	return Buffer.byteLength(JSON.stringify(entry.payload));
}

function isScalar(value: unknown): boolean {
	return (
		value === null ||
		typeof value === "string" ||
		typeof value === "number" ||
		typeof value === "boolean"
	);
}

export function truncatePayload(entry: MirrorEntryInput): MirrorEntryInput {
	const payload = entry.payload;
	const kept: Record<string, unknown> = {};
	if (typeof payload === "object" && payload !== null) {
		for (const [key, value] of Object.entries(payload)) {
			if (isScalar(value)) kept[key] = value;
		}
	}
	kept.truncated = true;
	return { ...entry, payload: kept };
}

export function isTerminalRejection(error: unknown): boolean {
	if (!(error instanceof CloudThinkerApiError)) return false;
	if (error.status === 401 || error.status === 429) return false;
	return error.status >= 400 && error.status < 500;
}

export function batch<T>(items: T[], size: number): T[][] {
	const batches: T[][] = [];
	for (let index = 0; index < items.length; index += size) {
		batches.push(items.slice(index, index + size));
	}
	return batches;
}

export interface MirrorStatus {
	pending: number;
	truncated: number;
	rejected: number;
	retryingAfterFailure: boolean;
}

export class SessionMirror {
	private acknowledged = new Set<string>();
	private truncated = new Set<string>();
	private rejected = new Set<string>();
	private conversationId: string | undefined;
	private linked = false;
	private backoffMs = 0;
	private nextAttemptAt = 0;
	private pendingCount = 0;
	private readonly client: CloudThinkerClient;
	private readonly onStatus: (status: MirrorStatus) => void;
	private readonly agentDir: string | undefined;
	private readonly now: () => number;
	readonly flush: (entries: SessionEntry[]) => Promise<void>;

	constructor(
		client: CloudThinkerClient,
		onStatus: (status: MirrorStatus) => void,
		agentDir?: string,
		now: () => number = Date.now,
	) {
		this.client = client;
		this.onStatus = onStatus;
		this.agentDir = agentDir;
		this.now = now;
		this.flush = singleFlight((entries: SessionEntry[]) => this.deliver(entries));
	}

	async link(conversationId: string): Promise<void> {
		this.conversationId = conversationId;
		this.linked = false;
		let acknowledged: Set<string>;
		try {
			acknowledged = await this.serverEntryIds(conversationId);
		} catch (error) {
			this.delayNextAttempt();
			this.report(true);
			throw error;
		}
		this.acknowledged = acknowledged;
		this.truncated = new Set();
		this.rejected = await this.readRejected(conversationId);
		this.linked = true;
		this.backoffMs = 0;
		this.nextAttemptAt = 0;
		this.pendingCount = 0;
	}

	unlink(): void {
		this.conversationId = undefined;
		this.linked = false;
	}

	sync(entries: SessionEntry[], onError: (error: unknown) => void): void {
		void this.flush(entries).catch(onError);
	}

	private async serverEntryIds(conversationId: string): Promise<Set<string>> {
		const acknowledged = new Set<string>();
		let afterSeq = 0;
		for (;;) {
			const page = await this.client.listEntries(conversationId, {
				after_seq: afterSeq,
				limit: ENTRY_PAGE_LIMIT,
			});
			for (const entry of page.entries) acknowledged.add(entry.entry_id);
			if (page.entries.length < ENTRY_PAGE_LIMIT) break;
			afterSeq = page.last_seq;
		}
		return acknowledged;
	}

	private delayNextAttempt(): void {
		this.backoffMs = Math.min(
			this.backoffMs === 0 ? FIRST_BACKOFF_MS : this.backoffMs * 2,
			MAX_BACKOFF_MS,
		);
		this.nextAttemptAt = this.now() + this.backoffMs;
	}

	private async deliver(entries: SessionEntry[]): Promise<void> {
		const conversationId = this.conversationId;
		if (!conversationId) return;
		if (!this.linked) {
			this.pendingCount = (await this.outstanding(conversationId, entries)).length;
			if (this.now() < this.nextAttemptAt) {
				this.report(true);
				return;
			}
			await this.link(conversationId);
		}
		const outstanding = await this.outstanding(conversationId, entries);
		this.pendingCount = outstanding.length;
		if (outstanding.length === 0) {
			await this.clearOutbox(conversationId);
			this.backoffMs = 0;
			this.nextAttemptAt = 0;
			this.report(false);
			return;
		}
		if (this.now() < this.nextAttemptAt) {
			this.report(true);
			return;
		}
		let sent = 0;
		for (const chunk of batch(outstanding, MAX_ENTRIES_PER_BATCH)) {
			try {
				await this.client.appendEntries(conversationId, chunk);
			} catch (error) {
				if (isTerminalRejection(error)) {
					for (const item of chunk) this.rejected.add(item.entry_id);
					await this.writeRejected(conversationId);
					sent += chunk.length;
					continue;
				}
				await this.writeOutbox(conversationId, outstanding.slice(sent));
				this.pendingCount = outstanding.length - sent;
				this.delayNextAttempt();
				this.report(true);
				throw error;
			}
			for (const entry of chunk) this.acknowledged.add(entry.entry_id);
			sent += chunk.length;
		}
		await this.clearOutbox(conversationId);
		this.pendingCount = 0;
		this.backoffMs = 0;
		this.nextAttemptAt = 0;
		this.report(false);
	}

	private async outstanding(
		conversationId: string,
		entries: SessionEntry[],
	): Promise<MirrorEntryInput[]> {
		const outstanding: MirrorEntryInput[] = [];
		const seen = new Set<string>();
		const consider = (candidate: MirrorEntryInput): void => {
			if (this.acknowledged.has(candidate.entry_id)) return;
			if (this.rejected.has(candidate.entry_id)) return;
			if (seen.has(candidate.entry_id)) return;
			seen.add(candidate.entry_id);
			if (payloadBytes(candidate) > MAX_PAYLOAD_BYTES) {
				this.truncated.add(candidate.entry_id);
				outstanding.push(truncatePayload(candidate));
				return;
			}
			outstanding.push(candidate);
		};
		for (const candidate of await this.readOutbox(conversationId)) consider(candidate);
		for (const entry of entries) consider(toMirrorEntry(entry));
		return outstanding;
	}

	private report(retryingAfterFailure: boolean): void {
		this.onStatus({
			pending: this.pendingCount,
			truncated: this.truncated.size,
			rejected: this.rejected.size,
			retryingAfterFailure,
		});
	}

	private async readOutbox(conversationId: string): Promise<MirrorEntryInput[]> {
		let raw: string;
		try {
			raw = await readFile(outboxPath(conversationId, this.agentDir), "utf8");
		} catch {
			return [];
		}
		const entries: MirrorEntryInput[] = [];
		for (const line of raw.split("\n")) {
			if (line.trim().length === 0) continue;
			try {
				entries.push(JSON.parse(line) as MirrorEntryInput);
			} catch {}
		}
		return entries;
	}

	private async writeOutbox(
		conversationId: string,
		entries: MirrorEntryInput[],
	): Promise<void> {
		const path = outboxPath(conversationId, this.agentDir);
		await mkdir(dirname(path), { recursive: true });
		await writeFile(
			path,
			`${entries.map((entry) => JSON.stringify(entry)).join("\n")}\n`,
			"utf8",
		);
	}

	private async clearOutbox(conversationId: string): Promise<void> {
		await rm(outboxPath(conversationId, this.agentDir), { force: true });
	}

	private async readRejected(conversationId: string): Promise<Set<string>> {
		try {
			const parsed: unknown = JSON.parse(
				await readFile(rejectedPath(conversationId, this.agentDir), "utf8"),
			);
			if (Array.isArray(parsed)) {
				return new Set(parsed.filter((item): item is string => typeof item === "string"));
			}
		} catch {}
		return new Set();
	}

	private async writeRejected(conversationId: string): Promise<void> {
		const path = rejectedPath(conversationId, this.agentDir);
		await mkdir(dirname(path), { recursive: true });
		await writeFile(path, JSON.stringify([...this.rejected]), "utf8");
	}
}

export function formatMirrorStatus(status: MirrorStatus): string | undefined {
	const parts: string[] = [];
	if (status.retryingAfterFailure) {
		parts.push(`✕ mirror offline, ${status.pending} pending`);
	}
	if (status.truncated > 0) parts.push(`${status.truncated} truncated`);
	if (status.rejected > 0) parts.push(`${status.rejected} rejected`);
	return parts.length > 0 ? parts.join(", ") : undefined;
}
