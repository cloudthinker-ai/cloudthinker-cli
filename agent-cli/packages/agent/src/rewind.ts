import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, readdirSync, renameSync, rmSync, statSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { isAbsolute, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { getAgentDir, type ExtensionAPI, type ExtensionCommandContext, type ExtensionContext, type SessionEntry } from "@earendil-works/pi-coding-agent";

import { cloudthinkerDir } from "@cloudthinker/cloud/src/paths.ts";

export const REWIND_TURNS = 20;
export const REWIND_IDLE_MS = 7 * 24 * 60 * 60 * 1000;
const FILE_TOOLS = new Set(["edit", "write"]);
const READ_TOOLS = new Set(["read", "grep", "find", "ls"]);

export interface FileChange {
	path: string;
	before: string | null;
	after: string | null;
}

export interface Checkpoint {
	startLeafId: string | null;
	startedAt: number;
	files: FileChange[];
	others: string[];
}

interface Ledger {
	version: 1;
	checkpoints: Checkpoint[];
}

export type RewindOutcome =
	| { kind: "empty" }
	| { kind: "conflict"; checkpoint: Checkpoint; paths: string[] }
	| { kind: "rewound"; checkpoint: Checkpoint; restored: string[]; removed: string[] };

export function rewindRoot(agentDir: string = getAgentDir()): string {
	return join(cloudthinkerDir(agentDir), "rewind");
}

function resolveToCwd(raw: string, cwd: string): string {
	let path = raw.replace(/[\u00A0\u2000-\u200A\u202F\u205F\u3000]/g, " ");
	if (path.startsWith("@")) path = path.slice(1);
	if (path === "~") path = homedir();
	else if (path.startsWith("~/")) path = join(homedir(), path.slice(2));
	else if (path.startsWith("file://")) path = fileURLToPath(path);
	return resolve(cwd, path);
}

function isMissing(error: unknown): boolean {
	return error instanceof Error && "code" in error && error.code === "ENOENT";
}

function digest(content: Buffer): string {
	return createHash("sha256").update(content).digest("hex");
}

function readBytes(path: string): Buffer | null {
	try {
		return readFileSync(path);
	} catch (error) {
		if (isMissing(error)) return null;
		throw error;
	}
}

function readLedger(path: string): Checkpoint[] {
	const raw = readBytes(path);
	if (raw === null) return [];
	let parsed: unknown;
	try {
		parsed = JSON.parse(raw.toString("utf8"));
	} catch (error) {
		if (error instanceof SyntaxError) return [];
		throw error;
	}
	if (typeof parsed !== "object" || parsed === null || !("version" in parsed) || parsed.version !== 1) return [];
	return "checkpoints" in parsed && Array.isArray(parsed.checkpoints) ? (parsed.checkpoints as Checkpoint[]) : [];
}

export class RewindStore {
	readonly dir: string;
	private checkpoints: Checkpoint[];
	private open: Checkpoint | undefined;

	constructor(dir: string) {
		this.dir = dir;
		this.checkpoints = readLedger(join(dir, "ledger.json"));
	}

	get turns(): readonly Checkpoint[] {
		return this.checkpoints;
	}

	begin(startLeafId: string | null, now: number = Date.now()): void {
		this.open = { startLeafId, startedAt: now, files: [], others: [] };
	}

	noteTool(toolName: string): void {
		const turn = this.open;
		if (!turn || READ_TOOLS.has(toolName) || FILE_TOOLS.has(toolName) || turn.others.includes(toolName)) return;
		turn.others.push(toolName);
	}

	before(path: string): void {
		const turn = this.open;
		if (!turn) return;
		if (turn.files.some((file) => file.path === path)) return;
		const before = this.keep(readBytes(path));
		turn.files.push({ path, before, after: before });
		this.save();
	}

	after(path: string): void {
		const file = this.open?.files.find((candidate) => candidate.path === path);
		if (!file) return;
		const content = readBytes(path);
		file.after = content === null ? null : digest(content);
		this.save();
	}

	end(): void {
		if (!this.open) return;
		this.open = undefined;
		this.save();
	}

	rewind(): RewindOutcome {
		const checkpoint = this.checkpoints.at(-1);
		if (!checkpoint) return { kind: "empty" };
		const pending: FileChange[] = [];
		const conflicts: string[] = [];
		for (const file of checkpoint.files) {
			const content = readBytes(file.path);
			const current = content === null ? null : digest(content);
			if (current === file.before) continue;
			if (current === file.after) pending.push(file);
			else conflicts.push(file.path);
		}
		if (conflicts.length > 0) return { kind: "conflict", checkpoint, paths: conflicts };
		const restored: string[] = [];
		const removed: string[] = [];
		for (const file of pending) {
			if (file.before === null) {
				rmSync(file.path, { force: true });
				removed.push(file.path);
			} else {
				writeFileSync(file.path, readFileSync(this.blob(file.before)));
				restored.push(file.path);
			}
		}
		this.checkpoints.pop();
		this.save(true);
		return { kind: "rewound", checkpoint, restored, removed };
	}

	private blob(hash: string): string {
		return join(this.dir, "blobs", hash);
	}

	private keep(content: Buffer | null): string | null {
		if (content === null) return null;
		const hash = digest(content);
		mkdirSync(join(this.dir, "blobs"), { recursive: true });
		writeFileSync(this.blob(hash), content, { mode: 0o600 });
		return hash;
	}

	private save(collect = false): void {
		const all = this.open ? [...this.checkpoints.filter((turn) => turn !== this.open), this.open] : this.checkpoints;
		const kept = all.slice(-REWIND_TURNS);
		const dropped = collect || kept.length < all.length;
		this.checkpoints = kept;
		mkdirSync(this.dir, { recursive: true, mode: 0o700 });
		const ledger = join(this.dir, "ledger.json");
		writeFileSync(`${ledger}.tmp`, JSON.stringify({ version: 1, checkpoints: this.checkpoints } satisfies Ledger), { mode: 0o600 });
		renameSync(`${ledger}.tmp`, ledger);
		if (dropped) this.collectBlobs();
	}

	private collectBlobs(): void {
		const live = new Set(this.checkpoints.flatMap((turn) => turn.files.map((file) => file.before)));
		mkdirSync(join(this.dir, "blobs"), { recursive: true });
		for (const name of readdirSync(join(this.dir, "blobs"))) {
			if (!live.has(name)) rmSync(this.blob(name), { force: true });
		}
	}
}

export function pruneIdle(root: string, keep: string, now: number = Date.now()): void {
	let names: string[];
	try {
		names = readdirSync(root);
	} catch (error) {
		if (isMissing(error)) return;
		throw error;
	}
	for (const name of names) {
		if (name === keep) continue;
		const dir = join(root, name);
		try {
			if (now - statSync(dir).mtimeMs > REWIND_IDLE_MS) rmSync(dir, { recursive: true, force: true });
		} catch (error) {
			if (!isMissing(error)) throw error;
		}
	}
}

export function rewindTarget(branch: readonly SessionEntry[], startLeafId: string | null): string | undefined {
	const start = startLeafId === null ? -1 : branch.findIndex((entry) => entry.id === startLeafId);
	if (startLeafId !== null && start < 0) return undefined;
	const prompt = branch.slice(start + 1).find((entry) => entry.type === "message" && entry.message.role === "user");
	if (prompt) return prompt.id;
	return startLeafId !== null && start < branch.length - 1 ? startLeafId : undefined;
}

function shown(path: string, cwd: string): string {
	const local = relative(cwd, path);
	return local && !local.startsWith("..") && !isAbsolute(local) ? local : path;
}

function rewindSummary(outcome: Extract<RewindOutcome, { kind: "rewound" }>, cwd: string): string {
	const list = (paths: readonly string[]) => paths.map((path) => shown(path, cwd)).join(", ");
	const parts: string[] = [];
	if (outcome.restored.length > 0) parts.push(`restored ${list(outcome.restored)}`);
	if (outcome.removed.length > 0) parts.push(`removed ${list(outcome.removed)} (the agent created ${outcome.removed.length === 1 ? "it" : "them"})`);
	return parts.length === 0 ? "Rewound the last turn: it changed no files." : `Rewound the last turn: ${parts.join("; ")}.`;
}

export function rewindMessage(outcome: RewindOutcome, cwd: string, rewound: boolean): string {
	if (outcome.kind === "empty") return "Nothing to rewind in this session.";
	if (outcome.kind === "conflict") {
		const paths = outcome.paths.map((path) => shown(path, cwd)).join(", ");
		return `Nothing rewound: ${paths} changed after the agent edited ${outcome.paths.length === 1 ? "it" : "them"}, and /rewind never overwrites your changes.`;
	}
	const lines = [rewindSummary(outcome, cwd)];
	if (outcome.checkpoint.others.length > 0) lines.push(`Not rewound: whatever ${outcome.checkpoint.others.join(", ")} changed in that turn.`);
	lines.push(rewound ? "The conversation is back to before that prompt." : "The conversation stays as it is, because that turn is not on this branch.");
	return lines.join("\n");
}

export function registerRewind(pi: ExtensionAPI, root: string = rewindRoot()): void {
	let store: RewindStore | undefined;
	const paths = new Map<string, string>();

	pi.on("session_start", (_event, ctx) => {
		const id = ctx.sessionManager.getSessionId();
		store = new RewindStore(join(root, id));
		pruneIdle(root, id);
	});
	pi.on("agent_start", (_event, ctx) => {
		store?.begin(ctx.sessionManager.getLeafId());
	});
	pi.on("tool_call", (event, ctx: ExtensionContext) => {
		const raw = (event.input as { path?: unknown }).path;
		const path = FILE_TOOLS.has(event.toolName) && typeof raw === "string" ? resolveToCwd(raw, ctx.cwd) : undefined;
		if (path !== undefined) paths.set(event.toolCallId, path);
		store?.noteTool(event.toolName);
		if (path !== undefined) store?.before(path);
	});
	pi.on("tool_result", (event) => {
		const path = paths.get(event.toolCallId);
		if (path === undefined) return;
		paths.delete(event.toolCallId);
		store?.after(path);
	});
	pi.on("agent_end", () => {
		store?.end();
	});
	pi.registerCommand("rewind", {
		description: "Rewind the last turn: restore the files it edited and the conversation",
		handler: async (_args, ctx: ExtensionCommandContext) => {
			if (!ctx.isIdle()) {
				ctx.ui.notify("Wait for the turn to finish (or press Esc), then /rewind.", "warning");
				return;
			}
			if (!store) return;
			const outcome = store.rewind();
			let rewound = false;
			if (outcome.kind === "rewound") {
				const target = rewindTarget(ctx.sessionManager.getBranch(), outcome.checkpoint.startLeafId);
				if (target !== undefined) rewound = !(await ctx.navigateTree(target)).cancelled;
			}
			ctx.ui.notify(rewindMessage(outcome, ctx.cwd, rewound), outcome.kind === "conflict" ? "warning" : "info");
		},
	});
}
