import { execFile } from "node:child_process";
import { basename } from "node:path";
import { promisify } from "node:util";

import { CombinedAutocompleteProvider, fuzzyMatch } from "@earendil-works/pi-tui";

const run = promisify(execFile);

export const INDEX_TTL_MS = 10_000;
export const MAX_FILE_SUGGESTIONS = 20;

export interface RepoIndex {
	entries: { path: string; isDirectory: boolean; lower: string }[];
	recent: Map<string, number>;
}

interface Suggestion {
	value: string;
	label: string;
	description: string;
}

interface FuzzyOptions {
	isQuotedPrefix: boolean;
	signal: AbortSignal;
}

interface PickerProvider {
	basePath: string;
	getFuzzyFileSuggestions(this: PickerProvider, query: string, options: FuzzyOptions): Promise<Suggestion[]>;
}

const cache = new Map<string, { at: number; index: Promise<RepoIndex | undefined> }>();

function paths(stdout: string): string[] {
	return stdout.split("\0").filter(Boolean);
}

async function git(cwd: string, args: string[]): Promise<string[]> {
	const { stdout } = await run("git", ["-C", cwd, ...args], { maxBuffer: 64 * 1024 * 1024 });
	return paths(stdout);
}

export async function buildRepoIndex(cwd: string): Promise<RepoIndex | undefined> {
	try {
		const [listed, deleted, changed, untracked, committed] = await Promise.all([
			git(cwd, ["ls-files", "-z", "--cached", "--others", "--exclude-standard"]),
			git(cwd, ["ls-files", "-z", "--deleted"]),
			git(cwd, ["diff", "--name-only", "--relative", "-z", "HEAD"]).catch(() => []),
			git(cwd, ["ls-files", "-z", "--others", "--exclude-standard"]),
			git(cwd, ["log", "-n", "30", "--name-only", "--relative", "-z", "--format="]).catch(() => []),
		]);
		const gone = new Set(deleted);
		const files = [...new Set(listed)].filter((path) => !gone.has(path));
		const present = new Set(files);
		const recent = new Map<string, number>();
		for (const path of [...changed, ...untracked, ...committed]) {
			if (present.has(path) && !recent.has(path)) recent.set(path, recent.size);
		}
		const directories = new Set<string>();
		for (const file of files) {
			for (let slash = file.indexOf("/"); slash !== -1; slash = file.indexOf("/", slash + 1)) directories.add(file.slice(0, slash));
		}
		return {
			entries: [...files.map((path) => ({ path, isDirectory: false })), ...[...directories].map((path) => ({ path, isDirectory: true }))]
				.map((entry) => ({ ...entry, lower: entry.path.toLowerCase() })),
			recent,
		};
	} catch {
		return undefined;
	}
}

function repoIndex(cwd: string, now = Date.now()): Promise<RepoIndex | undefined> {
	const cached = cache.get(cwd);
	if (cached && now - cached.at < INDEX_TTL_MS) return cached.index;
	const index = buildRepoIndex(cwd);
	cache.set(cwd, { at: now, index });
	return index;
}

function subsequence(query: string, text: string): boolean {
	let found = 0;
	for (let index = 0; index < text.length && found < query.length; index += 1) {
		if (text.charCodeAt(index) === query.charCodeAt(found)) found += 1;
	}
	return found === query.length;
}

export function rankFiles(index: RepoIndex, query: string): { path: string; isDirectory: boolean }[] {
	if (!query) {
		return [...index.recent.entries()].sort((left, right) => left[1] - right[1]).slice(0, MAX_FILE_SUGGESTIONS).map(([path]) => ({ path, isDirectory: false }));
	}
	const lower = query.toLowerCase();
	const scored: { entry: { path: string; isDirectory: boolean }; score: number }[] = [];
	for (const entry of index.entries) {
		if (!subsequence(lower, entry.lower)) continue;
		if (lower.length <= 2 && !entry.lower.slice(entry.lower.lastIndexOf("/") + 1).includes(lower)) continue;
		const name = basename(entry.path);
		const inName = fuzzyMatch(query, name);
		const match = inName.matches ? inName : fuzzyMatch(query, entry.path);
		if (!match.matches) continue;
		const lowerName = name.toLowerCase();
		const rank = index.recent.get(entry.path);
		const nameBonus = (inName.matches ? 100 : 0) + (lowerName === lower ? 60 : lowerName.startsWith(lower) ? 40 : lowerName.includes(lower) ? 25 : 0);
		const recentBonus = rank === undefined ? 0 : 30 - Math.min(rank, 25);
		scored.push({ entry, score: match.score - nameBonus - recentBonus });
	}
	scored.sort((left, right) => left.score - right.score || left.entry.path.length - right.entry.path.length);
	return scored.slice(0, MAX_FILE_SUGGESTIONS).map(({ entry }) => ({ path: entry.path, isDirectory: entry.isDirectory }));
}

export function suggestion(entry: { path: string; isDirectory: boolean }, quoted: boolean): Suggestion {
	const completion = entry.isDirectory ? `${entry.path}/` : entry.path;
	const value = quoted || completion.includes(" ") ? `@"${completion}"` : `@${completion}`;
	return { value, label: basename(entry.path) + (entry.isDirectory ? "/" : ""), description: entry.path };
}

export function applyFilePicker(): void {
	const prototype = CombinedAutocompleteProvider.prototype as unknown as PickerProvider;
	const original = prototype.getFuzzyFileSuggestions;
	if (typeof original !== "function") {
		throw new Error("pi-tui no longer exposes getFuzzyFileSuggestions, so @ cannot rank git files");
	}
	prototype.getFuzzyFileSuggestions = async function (query, options) {
		if (query.includes("/") || query.startsWith(".") || query.startsWith("~")) return original.call(this, query, options);
		const index = await repoIndex(this.basePath);
		if (options.signal.aborted) return [];
		const ranked = index ? rankFiles(index, query) : [];
		if (ranked.length === 0) return original.call(this, query, options);
		return ranked.map((entry) => suggestion(entry, options.isQuotedPrefix));
	};
}
