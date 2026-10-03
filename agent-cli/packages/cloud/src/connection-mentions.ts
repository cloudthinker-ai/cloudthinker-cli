import type { ConnectionEntry } from "./client.ts";

export const CONNECTION_MENTIONS_ENTRY = "ct_connection_mentions";

const SAFE_SEGMENT = /^[a-z0-9][a-z0-9_-]*$/i;

export interface ConnectionMention {
	token: string;
	prefix: string;
	alias: string;
	description: string;
}

let source: () => ConnectionEntry[] = () => [];

export function setConnectionSource(next: () => ConnectionEntry[]): void {
	source = next;
}

export function connectionMentions(entries: ConnectionEntry[] = source()): ConnectionMention[] {
	const safe = entries.filter((entry) => SAFE_SEGMENT.test(entry.prefix));
	const perPrefix = new Map<string, number>();
	for (const entry of safe) perPrefix.set(entry.prefix.toLowerCase(), (perPrefix.get(entry.prefix.toLowerCase()) ?? 0) + 1);
	return safe.map((entry) => {
		const prefix = entry.prefix.toLowerCase();
		const id = (perPrefix.get(prefix) ?? 0) > 1 && SAFE_SEGMENT.test(entry.alias) ? `${prefix}/${entry.alias}` : prefix;
		return { token: `#connection/${id}`, prefix, alias: entry.alias, description: entry.description };
	});
}

export const CONNECTION_MENTION = /(^|[\s(])#connection\/([A-Za-z0-9][\w-]*)(?:\/([A-Za-z0-9][\w-]*))?(?=$|[\s.,;:!?)])/g;

export interface ResolvedMention {
	token: string;
	prefix: string;
	alias: string | undefined;
	status: "connected" | "ambiguous" | "unknown";
	aliases: string[];
}

export function resolveConnectionMention(prefix: string, alias: string | undefined, entries: ConnectionEntry[] = source()): ResolvedMention {
	const lower = prefix.toLowerCase();
	const family = entries.filter((entry) => entry.prefix.toLowerCase() === lower);
	const aliases = family.map((entry) => entry.alias).filter(Boolean);
	const token = `#connection/${lower}${alias ? `/${alias}` : ""}`;
	if (alias) return { token, prefix: lower, alias, status: family.some((entry) => entry.alias === alias) ? "connected" : "unknown", aliases };
	return { token, prefix: lower, alias, status: family.length === 0 ? "unknown" : family.length === 1 ? "connected" : "ambiguous", aliases };
}

export function resolveConnectionMentions(text: string, entries: ConnectionEntry[] = source()): ResolvedMention[] {
	const seen = new Map<string, ResolvedMention>();
	for (const match of text.matchAll(CONNECTION_MENTION)) {
		const resolved = resolveConnectionMention(match[2]!, match[3], entries);
		seen.set(resolved.token, resolved);
	}
	return [...seen.values()];
}

export function mentionContext(mentions: ResolvedMention[]): string {
	const lines = mentions.map((mention) => {
		const list = `connection_list=["${mention.prefix}"]`;
		if (mention.status === "connected") return `- ${mention.token}: connected${mention.alias ? `, instance ${mention.alias}` : mention.aliases[0] ? `, instance ${mention.aliases[0]}` : ""}. Use ${list}.`;
		if (mention.status === "ambiguous") return `- ${mention.token}: matches ${mention.aliases.length} Connections (${mention.aliases.join(", ")}). Use ${list} and select the instance the user means, or ask which one.`;
		return `- ${mention.token}: not connected in this workspace. Say so instead of using another Connection.`;
	});
	return `The user's message mentions these Connections:\n${lines.join("\n")}`;
}
