import type { SessionEntry } from "@earendil-works/pi-coding-agent";

import { sanitizeTerminalText } from "./awareness.ts";
import type { SessionCredits } from "./client.ts";
import { formatCredits } from "./credits.ts";

interface EntryUsage {
	input: number;
	output: number;
	cacheRead: number;
	cacheWrite: number;
}

export interface SessionTotals {
	messages: number;
	user: number;
	assistant: number;
	toolCalls: number;
	toolResults: number;
	input: number;
	output: number;
	cacheRead: number;
	cacheWrite: number;
}

function emptyTotals(): SessionTotals {
	return {
		messages: 0,
		user: 0,
		assistant: 0,
		toolCalls: 0,
		toolResults: 0,
		input: 0,
		output: 0,
		cacheRead: 0,
		cacheWrite: 0,
	};
}

function add(totals: SessionTotals, usage: EntryUsage | undefined): void {
	if (!usage) return;
	totals.input += usage.input ?? 0;
	totals.output += usage.output ?? 0;
	totals.cacheRead += usage.cacheRead ?? 0;
	totals.cacheWrite += usage.cacheWrite ?? 0;
}

export function sessionTotals(entries: SessionEntry[]): SessionTotals {
	const totals = emptyTotals();
	for (const entry of entries) {
		if (entry.type === "branch_summary" || entry.type === "compaction") {
			add(totals, entry.usage as EntryUsage | undefined);
			continue;
		}
		if (entry.type !== "message") continue;
		totals.messages += 1;
		const message = entry.message;
		if (message.role === "user") {
			totals.user += 1;
			continue;
		}
		if (message.role === "toolResult") {
			totals.toolResults += 1;
			add(totals, message.usage as EntryUsage | undefined);
			continue;
		}
		if (message.role === "assistant") {
			totals.assistant += 1;
			if (Array.isArray(message.content)) {
				totals.toolCalls += message.content.filter(
					(block) => block.type === "toolCall",
				).length;
			}
			add(totals, message.usage as EntryUsage | undefined);
		}
	}
	return totals;
}

export interface SessionIdentity {
	file: string | undefined;
	id: string;
	name: string | undefined;
	webUrl: string | undefined;
}

function count(value: number): string {
	return value.toLocaleString("en-US");
}

export function sessionPanelLines(
	identity: SessionIdentity,
	totals: SessionTotals,
	credits: SessionCredits | undefined,
): string[] {
	const prompt = totals.input + totals.cacheRead + totals.cacheWrite;
	const lines = ["Session"];
	if (identity.name) lines.push(`Name: ${sanitizeTerminalText(identity.name)}`);
	lines.push(`File: ${identity.file === undefined ? "in memory" : sanitizeTerminalText(identity.file)}`);
	lines.push(`ID: ${sanitizeTerminalText(identity.id)}`);
	lines.push(identity.webUrl ? `Mirror: ${sanitizeTerminalText(identity.webUrl)}` : "Mirror: not linked");
	lines.push("");
	lines.push("Messages");
	lines.push(`Total: ${count(totals.messages)}`);
	lines.push(`User: ${count(totals.user)}`);
	lines.push(`Assistant: ${count(totals.assistant)}`);
	lines.push(`Tools: ${count(totals.toolCalls)} calls, ${count(totals.toolResults)} results`);
	lines.push("");
	lines.push("Tokens");
	lines.push(`Input: ${count(prompt)}`);
	if (prompt > 0 && totals.cacheRead + totals.cacheWrite > 0) {
		lines.push(`  Cached: ${count(totals.cacheRead)}`);
		lines.push(`  Uncached: ${count(totals.input + totals.cacheWrite)}`);
	}
	lines.push(`Output: ${count(totals.output)}`);
	lines.push(`Total: ${count(prompt + totals.output)}`);
	lines.push("");
	lines.push("Credits");
	if (!credits) {
		lines.push("This workspace's ledger has not answered yet.");
		return lines;
	}
	lines.push(`Charged: ${formatCredits(credits.credits_used)}`);
	lines.push(`Billed by the gateway: ${count(credits.tokens_consumed)} tokens`);
	return lines;
}
