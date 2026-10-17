import { appendFile, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { dirname } from "node:path";

import type { SessionEntry } from "@earendil-works/pi-coding-agent";

import { singleFlight } from "./async.ts";
import { CloudThinkerApiError } from "./client.ts";
import type { CloudThinkerClient, MirrorEntryInput } from "./client.ts";
import { conversationPathSegment } from "./conversation-id.ts";
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

interface DeliveryRequest {
	conversationId: string | undefined;
	generation: number;
}

export class SessionMirror {
	private acknowledged = new Set<string>();
	private truncated = new Set<string>();
	private rejected = new Set<string>();
	private conversationId: string | undefined;
	private linked = false;
	private linkGeneration = 0;
	private backoffMs = 0;
	private nextAttemptAt = 0;
	private pendingCount = 0;
	private readonly pendingEntryIds = new Set<string>();
	private outboxConversationId: string | undefined;
	private readonly outboxEntries = new Map<string, MirrorEntryInput>();
	private readonly client: CloudThinkerClient;
	private readonly onStatus: (status: MirrorStatus) => void;
	private readonly agentDir: string | undefined;
	private readonly now: () => number;
	private outboxOperations: Promise<void> = Promise.resolve();
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
		const deliver = singleFlight((request: DeliveryRequest) => this.deliver(request));
		this.flush = async (entries: SessionEntry[]) => {
			const conversationId = this.conversationId;
			const generation = this.linkGeneration;
			if (conversationId) await this.stageEntries(conversationId, entries, generation);
			await deliver({ conversationId, generation });
		};
	}

	async link(conversationId: string): Promise<void> {
		conversationPathSegment(conversationId);
		if (this.conversationId !== conversationId) {
			this.acknowledged.clear();
			this.truncated.clear();
			this.rejected.clear();
			this.pendingEntryIds.clear();
			this.outboxEntries.clear();
			this.outboxConversationId = undefined;
			this.pendingCount = 0;
			this.backoffMs = 0;
			this.nextAttemptAt = 0;
		}
		const generation = ++this.linkGeneration;
		this.conversationId = conversationId;
		this.linked = false;
		let acknowledged: Set<string>;
		try {
			acknowledged = await this.serverEntryIds(conversationId);
		} catch (error) {
			if (!this.isCurrentLink(generation, conversationId)) return;
			this.delayNextAttempt();
			this.report(true);
			throw error;
		}
		if (!this.isCurrentLink(generation, conversationId)) return;
		const rejected = await this.readRejected(conversationId);
		if (!this.isCurrentLink(generation, conversationId)) return;
		this.acknowledged = acknowledged;
		this.truncated = new Set();
		this.rejected = rejected;
		for (const entryId of this.pendingEntryIds) {
			if (this.acknowledged.has(entryId) || this.rejected.has(entryId)) {
				this.pendingEntryIds.delete(entryId);
			}
		}
		this.pendingCount = this.pendingEntryIds.size;
		this.linked = true;
		this.backoffMs = 0;
		this.nextAttemptAt = 0;
	}

	unlink(): void {
		this.linkGeneration += 1;
		this.conversationId = undefined;
		this.linked = false;
		this.acknowledged = new Set();
		this.truncated = new Set();
		this.rejected = new Set();
		this.backoffMs = 0;
		this.nextAttemptAt = 0;
		this.pendingCount = 0;
		this.pendingEntryIds.clear();
		this.outboxEntries.clear();
		this.outboxConversationId = undefined;
		this.report(false);
	}

	private isCurrentLink(generation: number, conversationId: string): boolean {
		return this.linkGeneration === generation && this.conversationId === conversationId;
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

	private async deliver(request: DeliveryRequest): Promise<void> {
		const { conversationId } = request;
		if (!conversationId) return;
		let generation = request.generation;
		if (!this.isCurrentLink(generation, conversationId)) return;
		if (!this.linked) {
			this.pendingCount = this.pendingEntryIds.size;
			if (this.now() < this.nextAttemptAt) {
				this.report(true);
				return;
			}
			const linking = this.link(conversationId);
			generation = this.linkGeneration;
			await linking;
			if (!this.isCurrentLink(generation, conversationId) || !this.linked) return;
		}
		if (!this.isCurrentLink(generation, conversationId)) return;
		const outstanding: MirrorEntryInput[] = [];
		for (const entryId of this.pendingEntryIds) {
			const entry = this.outboxEntries.get(entryId);
			if (entry) outstanding.push(entry);
		}
		this.pendingCount = this.pendingEntryIds.size;
		if (outstanding.length === 0) {
			await this.clearOutbox(conversationId, generation);
			if (!this.isCurrentLink(generation, conversationId)) return;
			this.backoffMs = 0;
			this.nextAttemptAt = 0;
			this.report(false);
			return;
		}
		if (this.now() < this.nextAttemptAt) {
			this.report(true);
			return;
		}
		const rejectedForDelivery = new Set(this.rejected);
		for (const chunk of batch(outstanding, MAX_ENTRIES_PER_BATCH)) {
			try {
				await this.client.appendEntries(conversationId, chunk);
			} catch (error) {
				if (isTerminalRejection(error)) {
					for (const item of chunk) rejectedForDelivery.add(item.entry_id);
					await this.writeRejected(conversationId, rejectedForDelivery);
					if (!this.isCurrentLink(generation, conversationId)) return;
					this.rejected = new Set(rejectedForDelivery);
					for (const item of chunk) this.pendingEntryIds.delete(item.entry_id);
					this.pendingCount = this.pendingEntryIds.size;
					continue;
				}
				if (!this.isCurrentLink(generation, conversationId)) throw error;
				this.pendingCount = this.pendingEntryIds.size;
				this.delayNextAttempt();
				this.report(true);
				throw error;
			}
			if (!this.isCurrentLink(generation, conversationId)) return;
			for (const entry of chunk) {
				this.acknowledged.add(entry.entry_id);
				this.pendingEntryIds.delete(entry.entry_id);
			}
			this.pendingCount = this.pendingEntryIds.size;
		}
		await this.clearOutbox(conversationId, generation);
		if (!this.isCurrentLink(generation, conversationId)) return;
		this.pendingCount = this.pendingEntryIds.size;
		this.backoffMs = 0;
		this.nextAttemptAt = 0;
		this.report(false);
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

	private async stageEntries(
		conversationId: string,
		entries: SessionEntry[],
		generation: number,
	): Promise<void> {
		return this.withOutboxOperation(async () => {
			if (!this.isCurrentLink(generation, conversationId)) return;
			if (this.outboxConversationId !== conversationId) {
				const outbox = await this.readOutbox(conversationId);
				if (!this.isCurrentLink(generation, conversationId)) return;
				this.outboxEntries.clear();
				for (const entry of outbox) {
					if (!this.outboxEntries.has(entry.entry_id)) {
						this.outboxEntries.set(entry.entry_id, entry);
						if (
							!this.acknowledged.has(entry.entry_id) &&
							!this.rejected.has(entry.entry_id)
						) {
							this.pendingEntryIds.add(entry.entry_id);
						}
					}
				}
				this.outboxConversationId = conversationId;
			}
			const missing: MirrorEntryInput[] = [];
			for (const entry of entries) {
				const candidate = toMirrorEntry(entry);
				if (
					this.acknowledged.has(candidate.entry_id) ||
					this.rejected.has(candidate.entry_id) ||
					this.outboxEntries.has(candidate.entry_id)
				) {
					continue;
				}
				let staged = candidate;
				if (payloadBytes(candidate) > MAX_PAYLOAD_BYTES) {
					this.truncated.add(candidate.entry_id);
					staged = truncatePayload(candidate);
				}
				missing.push(staged);
			}
			await this.appendOutboxEntries(conversationId, missing);
			if (!this.isCurrentLink(generation, conversationId)) return;
			for (const entry of missing) {
				this.outboxEntries.set(entry.entry_id, entry);
				this.pendingEntryIds.add(entry.entry_id);
			}
			this.pendingCount = this.pendingEntryIds.size;
		});
	}

	private async clearOutbox(conversationId: string, generation: number): Promise<void> {
		await this.withOutboxOperation(async () => {
			if (!this.isCurrentLink(generation, conversationId)) return;
			this.pendingCount = this.pendingEntryIds.size;
			if (this.pendingCount > 0) {
				return;
			}
			await rm(outboxPath(conversationId, this.agentDir), { force: true });
			if (this.outboxConversationId === conversationId) this.outboxEntries.clear();
		});
	}

	private async appendOutboxEntries(
		conversationId: string,
		entries: MirrorEntryInput[],
	): Promise<void> {
		if (entries.length === 0) return;
		const path = outboxPath(conversationId, this.agentDir);
		await mkdir(dirname(path), { recursive: true });
		const lines = entries.map((entry) => JSON.stringify(entry)).join("\n");
		await appendFile(
			path,
			`\n${lines}\n`,
			"utf8",
		);
	}

	private async withOutboxOperation<T>(operation: () => Promise<T>): Promise<T> {
		const previous = this.outboxOperations;
		let release!: () => void;
		this.outboxOperations = new Promise<void>((resolve) => {
			release = resolve;
		});
		await previous;
		try {
			return await operation();
		} finally {
			release();
		}
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

	private async writeRejected(
		conversationId: string,
		rejected: Set<string>,
	): Promise<void> {
		const path = rejectedPath(conversationId, this.agentDir);
		await mkdir(dirname(path), { recursive: true });
		await writeFile(path, JSON.stringify([...rejected]), "utf8");
	}
}

export function formatMirrorStatus(status: MirrorStatus): string | undefined {
	const parts: string[] = [];
	if (status.retryingAfterFailure) {
		parts.push(`mirror offline (${status.pending} pending)`);
	}
	if (status.truncated > 0) parts.push(`${status.truncated} truncated`);
	if (status.rejected > 0) parts.push(`${status.rejected} rejected`);
	return parts.length > 0 ? parts.join(", ") : undefined;
}
