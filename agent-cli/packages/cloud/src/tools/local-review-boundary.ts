import { spawnSync } from "node:child_process";
import { realpathSync } from "node:fs";
import { realpath } from "node:fs/promises";
import { isAbsolute, relative, resolve, sep } from "node:path";

import type { ExtensionAPI, ToolCallEvent, ToolResultEvent } from "@earendil-works/pi-coding-agent";

const FILE_TOOLS = new Set(["read", "grep", "find", "ls"]);
const RESTRICTED_PATH_REASON = "Local review can access only existing, non-ignored paths inside this checkout.";

function containsGitMetadata(path: string): boolean {
	return path.split(/[\\/]/).includes(".git");
}

function isWithin(root: string, candidate: string): boolean {
	const pathFromRoot = relative(root, candidate);
	return pathFromRoot === "" || (pathFromRoot !== ".." && !pathFromRoot.startsWith(`..${sep}`) && !isAbsolute(pathFromRoot));
}

function isGitIgnored(root: string, path: string): boolean {
	const result = spawnSync("git", ["check-ignore", "--quiet", "--no-index", "--", path], {
		cwd: root,
		stdio: "ignore",
	});
	if (result.status === 0) return true;
	if (result.status === 1) return false;
	throw result.error ?? new Error("Could not apply Git ignore rules to local review access.");
}

function gitIgnoredPaths(root: string, paths: string[]): Set<string> {
	if (paths.length === 0) return new Set();
	const result = spawnSync("git", ["check-ignore", "--no-index", "--stdin"], {
		cwd: root,
		input: paths.join("\n"),
		encoding: "utf8",
	});
	if (result.status === 0) return new Set(result.stdout.split("\n").filter(Boolean));
	if (result.status === 1) return new Set();
	throw result.error ?? new Error("Could not apply Git ignore rules to local review access.");
}

async function resolveReviewPath(root: string, requested: unknown): Promise<string> {
	if (requested !== undefined && typeof requested !== "string") throw new Error(RESTRICTED_PATH_REASON);
	const lexicalPath = resolve(root, requested || ".");
	if (!isWithin(root, lexicalPath) || containsGitMetadata(relative(root, lexicalPath))) {
		throw new Error(RESTRICTED_PATH_REASON);
	}
	const resolvedPath = await realpath(lexicalPath);
	if (!isWithin(root, resolvedPath) || containsGitMetadata(relative(root, resolvedPath))) {
		throw new Error(RESTRICTED_PATH_REASON);
	}
	if (isGitIgnored(root, relative(root, resolvedPath) || ".")) throw new Error(RESTRICTED_PATH_REASON);
	return resolvedPath;
}

function containedRelativePath(root: string, base: string, candidate: string): string | undefined {
	if (!candidate || containsGitMetadata(candidate)) return undefined;
	const lexicalPath = resolve(base, candidate);
	if (!isWithin(root, lexicalPath) || containsGitMetadata(relative(root, lexicalPath))) return undefined;
	return relative(root, lexicalPath) || ".";
}

function listingPath(root: string, base: string, line: string): string | undefined {
	const candidate = line.trim().replace(/^[│├└─\s]+/u, "").replace(/\/$/, "");
	return containedRelativePath(root, base, candidate);
}

function grepResultPath(root: string, base: string, line: string): string | undefined {
	const match = /^(.*?)(?::|-)(\d+)(?::|-)\s/.exec(line);
	return match ? containedRelativePath(root, base, match[1] ?? "") : undefined;
}

function withoutPrivateEntries(
	content: ToolResultEvent["content"],
	root: string,
	pathFromLine: (line: string) => string | undefined,
	keepUnparsed: boolean,
): ToolResultEvent["content"] {
	const linesByBlock = content.map((block) => block.type === "text" ? block.text.split("\n") : undefined);
	const paths = linesByBlock.flatMap((lines) => lines?.map(pathFromLine).filter((item): item is string => item !== undefined) ?? []);
	const ignored = gitIgnoredPaths(root, paths);
	return content.map((block, index) => {
		const lines = linesByBlock[index];
		if (block.type !== "text" || !lines) return block;
		const visibleLines = lines.filter((line) => {
			const candidatePath = pathFromLine(line);
			return candidatePath === undefined ? keepUnparsed : !ignored.has(candidatePath);
		});
		return { ...block, text: visibleLines.join("\n") || "(no visible entries)" };
	});
}

function toolResultBase(root: string, event: ToolResultEvent): string {
	const requested = (event.input as { path?: unknown }).path;
	if (typeof requested !== "string") return root;
	const base = resolve(root, requested);
	return isWithin(root, base) ? base : root;
}

export function registerLocalReviewBoundary(pi: ExtensionAPI, cwd: string = process.cwd()): void {
	const root = realpathSync(resolve(cwd));
	pi.on("tool_call", async (event: ToolCallEvent) => {
		if (!FILE_TOOLS.has(event.toolName)) return;
		try {
			const input = event.input as { path?: unknown };
			input.path = await resolveReviewPath(root, input.path);
		} catch {
			return { block: true, reason: RESTRICTED_PATH_REASON };
		}
	});
	pi.on("tool_result", (event: ToolResultEvent) => {
		if (event.isError) return;
		if (event.toolName !== "find" && event.toolName !== "ls" && event.toolName !== "grep") return;
		try {
			const base = toolResultBase(root, event);
			const pathFromLine = event.toolName === "grep"
				? (line: string) => grepResultPath(root, base, line)
				: (line: string) => listingPath(root, base, line);
			return { content: withoutPrivateEntries(event.content, root, pathFromLine, event.toolName === "grep") };
		} catch {
			return { content: [{ type: "text", text: "(no visible entries)" }] };
		}
	});
}
