import type { SessionEntry } from "@earendil-works/pi-coding-agent";

import type { MirrorEntryInput } from "./client.ts";

export const MAX_PAYLOAD_BYTES = 1024 * 1024;

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

const CUT_MARKER_ROOM = 64;

function isScalar(value: unknown): boolean {
	return (
		value === null ||
		typeof value === "string" ||
		typeof value === "number" ||
		typeof value === "boolean"
	);
}

interface StringLeaf {
	parent: Record<string, unknown> | unknown[];
	key: string;
}

function stringLeaves(value: unknown, leaves: StringLeaf[] = []): StringLeaf[] {
	if (typeof value !== "object" || value === null) return leaves;
	const record = value as Record<string, unknown>;
	for (const key of Object.keys(record)) {
		const child = record[key];
		if (typeof child === "string") leaves.push({ parent: record, key });
		else stringLeaves(child, leaves);
	}
	return leaves;
}

function leafText(leaf: StringLeaf): string {
	return (leaf.parent as Record<string, string>)[leaf.key]!;
}

function cutText(text: string, keepChars: number): string {
	let head = text.slice(0, Math.ceil(keepChars / 2));
	let tail = text.slice(text.length - Math.floor(keepChars / 2));
	if (/[\ud800-\udbff]$/.test(head)) head = head.slice(0, -1);
	if (/^[\udc00-\udfff]/.test(tail)) tail = tail.slice(1);
	const cutBytes = Buffer.byteLength(text) - Buffer.byteLength(head) - Buffer.byteLength(tail);
	return `${head}\n… ${cutBytes} bytes cut …\n${tail}`;
}

export function truncatePayload(entry: MirrorEntryInput): MirrorEntryInput {
	const source = entry.payload;
	if (typeof source !== "object" || source === null) {
		return { ...entry, payload: { truncated: true } };
	}
	const payload = { ...(structuredClone(source) as Record<string, unknown>), truncated: true };
	const leaves = stringLeaves(payload).sort((a, b) => leafText(b).length - leafText(a).length);
	let size = Buffer.byteLength(JSON.stringify(payload));
	for (const leaf of leaves) {
		if (size <= MAX_PAYLOAD_BYTES) break;
		const text = leafText(leaf);
		const keepChars = Math.max(0, text.length - (size - MAX_PAYLOAD_BYTES) - CUT_MARKER_ROOM);
		(leaf.parent as Record<string, string>)[leaf.key] = cutText(text, keepChars);
		size = Buffer.byteLength(JSON.stringify(payload));
	}
	if (size <= MAX_PAYLOAD_BYTES) return { ...entry, payload };
	const kept: Record<string, unknown> = {};
	for (const [key, value] of Object.entries(source)) {
		if (isScalar(value) && (typeof value !== "string" || value.length <= CUT_MARKER_ROOM)) kept[key] = value;
	}
	kept.truncated = true;
	return { ...entry, payload: kept };
}
