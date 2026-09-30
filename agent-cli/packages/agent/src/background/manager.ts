import { createHash, randomUUID } from "node:crypto";
import { chmod, mkdir, readFile, rename, writeFile } from "node:fs/promises";
import { join } from "node:path";
import type { BashOperations } from "@earendil-works/pi-coding-agent";
import lockfile from "proper-lockfile";

export const BACKGROUND_MAX_OUTPUT_BYTES = 128 * 1024;
export const BACKGROUND_MAX_READ_BYTES = 16 * 1024;
export const BACKGROUND_MAX_TASKS = 100;
export const BACKGROUND_DEFAULT_TIMEOUT_SECONDS = 1800;

export type BackgroundTaskState = "running" | "succeeded" | "failed" | "cancelled" | "timed_out" | "interrupted";

export interface BackgroundTask {
	id: string;
	command: string;
	cwd: string;
	state: BackgroundTaskState;
	createdAt: number;
	finishedAt?: number;
	exitCode?: number | null;
	outputBaseByte: number;
	totalOutputBytes: number;
	output: string;
	completionDelivered: boolean;
	persistenceErrorReported?: boolean;
}

export type BackgroundTaskSummary = Omit<BackgroundTask, "output" | "outputBaseByte" | "completionDelivered" | "persistenceErrorReported">;

export interface BackgroundOutputRead {
	text: string;
	startByte: number;
	nextByte: number;
	totalBytes: number;
	truncated: boolean;
	droppedBytes: number;
}

interface StoredState {
	version: 1;
	tasks: BackgroundTask[];
}

export interface BackgroundManagerOptions {
	storageDirectory: string;
	cwd: string;
	operations: BashOperations;
	onTerminal?: (task: BackgroundTaskSummary) => void | Promise<void>;
	onChange?: (tasks: readonly BackgroundTaskSummary[]) => void;
}

interface RunningProcess {
	controller: AbortController;
	processDone: Promise<void>;
}

const terminalStates = new Set<BackgroundTaskState>(["succeeded", "failed", "cancelled", "timed_out", "interrupted"]);

function isTerminal(state: BackgroundTaskState): boolean {
	return terminalStates.has(state);
}

function isStoredState(value: unknown): value is StoredState {
	if (typeof value !== "object" || value === null) return false;
	const state = value as Partial<StoredState>;
	return state.version === 1 && Array.isArray(state.tasks);
}

function utf8Boundary(bytes: Buffer, start: number): number {
	let boundary = start;
	while (boundary < bytes.length && (bytes[boundary]! & 0xc0) === 0x80) boundary++;
	return boundary;
}

export class BackgroundCommandManager {
	private readonly tasks = new Map<string, BackgroundTask>();
	private readonly running = new Map<string, RunningProcess>();
	private readonly completions: string[] = [];
	private readonly recoveryNotices: string[] = [];
	private initialized = false;
	private closed = false;
	private shutdownPromise: Promise<void> | undefined;
	private compromised = false;
	private releaseLock: (() => Promise<void>) | undefined;
	private writeChain: Promise<void> = Promise.resolve();
	private persistTimer: NodeJS.Timeout | undefined;
	private readonly options: BackgroundManagerOptions;

	constructor(options: BackgroundManagerOptions) {
		this.options = options;
	}

	static sessionStorageDirectory(agentDirectory: string, sessionId: string): string {
		const key = createHash("sha256").update(sessionId).digest("hex");
		return join(agentDirectory, "background", key);
	}

	async initialize(): Promise<void> {
		if (this.initialized) return;
		await mkdir(this.options.storageDirectory, { recursive: true, mode: 0o700 });
		await chmod(this.options.storageDirectory, 0o700);
		try {
			this.releaseLock = await lockfile.lock(this.options.storageDirectory, {
				realpath: false,
				stale: 30000,
				update: 10000,
				onCompromised: (error) => this.onLockCompromised(error),
			});
		} catch (error) {
			if (isLocked(error)) throw new Error("Background commands for this session are already active in another process");
			throw error;
		}
		try {
			const value: unknown = JSON.parse(await readFile(this.statePath(), "utf8"));
			if (!isStoredState(value)) throw new Error("Background task state file has an invalid format");
			for (const task of value.tasks) {
				if (!this.validTask(task)) continue;
				const recovered = task.state === "running"
					? { ...task, state: "interrupted" as const, finishedAt: Date.now(), completionDelivered: false }
					: task;
				this.tasks.set(recovered.id, recovered);
				if (task.state === "running" || !task.completionDelivered) {
					this.recoveryNotices.push(`${recovered.id}: ${recovered.state} — ${recovered.command}`);
				}
			}
		} catch (error) {
			if (!isMissingFile(error)) {
				await this.releaseLock?.();
				this.releaseLock = undefined;
				throw error;
			}
		}
		try {
			await this.persist();
			this.initialized = true;
		} catch (error) {
			await this.releaseLock?.();
			this.releaseLock = undefined;
			throw error;
		}
	}

	async start(command: string, cwd = this.options.cwd, timeoutSeconds?: number): Promise<BackgroundTaskSummary> {
		this.ensureReady();
		if (this.closed) throw new Error("Background command manager is shutting down");
		if (command.trim().length === 0) throw new Error("command must not be empty");
		if (timeoutSeconds !== undefined && (!Number.isFinite(timeoutSeconds) || timeoutSeconds <= 0 || timeoutSeconds > 86400)) {
			throw new Error("timeoutSeconds must be greater than 0 and at most 86400");
		}
		this.pruneTasks();
		if (this.tasks.size >= BACKGROUND_MAX_TASKS) throw new Error(`At most ${BACKGROUND_MAX_TASKS} background commands can be retained in one session`);
		const task: BackgroundTask = {
			id: randomUUID().replaceAll("-", "").slice(0, 12),
			command,
			cwd,
			state: "running",
			createdAt: Date.now(),
			outputBaseByte: 0,
			totalOutputBytes: 0,
			output: "",
			completionDelivered: false,
		};
		this.tasks.set(task.id, task);
		this.changed();
		try {
			await this.persist();
		} catch (error) {
			this.tasks.delete(task.id);
			this.changed();
			throw error;
		}
		if (this.closed) {
			task.state = "cancelled";
			task.finishedAt = Date.now();
			this.changed();
			await this.persist();
			return this.publicTask(task);
		}
		const controller = new AbortController();
		const processDone = Promise.resolve().then(() => this.run(task, controller, timeoutSeconds ?? BACKGROUND_DEFAULT_TIMEOUT_SECONDS));
		this.running.set(task.id, { controller, processDone });
		this.changed();
		return this.publicTask(task);
	}

	list(): BackgroundTaskSummary[] {
		this.ensureReady();
		return [...this.tasks.values()].map((task) => this.publicTask(task));
	}

	takeRecoveryNotices(): string[] {
		const notices = this.recoveryNotices.splice(0);
		if (notices.length > 0) {
			for (const task of this.tasks.values()) {
				if (task.state !== "running") task.completionDelivered = true;
			}
			void this.persist().catch((error: unknown) => process.emitWarning(`Unable to save background recovery status: ${error instanceof Error ? error.message : String(error)}`));
		}
		return notices;
	}

	get(taskId: string): BackgroundTaskSummary {
		this.ensureReady();
		const task = this.requireTask(taskId);
		return this.publicTask(task);
	}

	readOutput(taskId: string, afterByte = 0, maxBytes = BACKGROUND_MAX_READ_BYTES): BackgroundOutputRead {
		this.ensureReady();
		if (!Number.isSafeInteger(afterByte) || afterByte < 0) throw new Error("afterByte must be a nonnegative integer");
		if (!Number.isSafeInteger(maxBytes) || maxBytes < 4) throw new Error("maxBytes must be an integer of at least 4");
		const task = this.requireTask(taskId);
		const bytes = Buffer.from(task.output, "base64");
		const max = Math.min(maxBytes, BACKGROUND_MAX_READ_BYTES);
		const totalBytes = task.totalOutputBytes;
		const requested = Math.min(afterByte, totalBytes);
		const retainedStart = Math.max(requested, task.outputBaseByte);
		const localStart = Math.max(0, retainedStart - task.outputBaseByte);
		const alignedStart = utf8Boundary(bytes, localStart);
		const end = Math.min(bytes.length, alignedStart + max);
		let alignedEnd = end;
		while (alignedEnd > alignedStart && alignedEnd < bytes.length && (bytes[alignedEnd]! & 0xc0) === 0x80) alignedEnd--;
		const nextByte = task.outputBaseByte + alignedEnd;
		return {
			text: bytes.subarray(alignedStart, alignedEnd).toString("utf8"),
			startByte: task.outputBaseByte + alignedStart,
			nextByte,
			totalBytes,
			truncated: afterByte < task.outputBaseByte,
			droppedBytes: Math.max(0, task.outputBaseByte + alignedStart - requested),
		};
	}

	async cancel(taskId: string): Promise<BackgroundTaskSummary> {
		this.ensureReady();
		if (this.compromised) throw new Error("Background command session ownership was lost");
		const task = this.requireTask(taskId);
		const running = this.running.get(task.id);
		if (running && task.state === "running") {
			task.state = "cancelled";
			task.finishedAt = Date.now();
			running.controller.abort();
			this.changed();
			await this.persist();
			await running.processDone;
			task.completionDelivered = true;
			await this.persist();
			return this.publicTask(task);
		}
		if (task.state === "cancelled" && !task.completionDelivered) {
			task.completionDelivered = true;
			await this.persist();
		}
		return this.publicTask(task);
	}

	async waitForActive(): Promise<void> {
		while (this.running.size > 0) {
			await Promise.all([...this.running.values()].map((running) => running.processDone));
		}
	}

	takeCompletions(): BackgroundTaskSummary[] {
		const tasks: BackgroundTaskSummary[] = [];
		for (const id of this.completions.splice(0)) {
			const task = this.tasks.get(id);
			if (!task || task.completionDelivered || !isTerminal(task.state) || task.state === "cancelled") continue;
			task.completionDelivered = true;
			tasks.push(this.publicTask(task));
		}
		if (tasks.length > 0) void this.persist().catch((error: unknown) => process.emitWarning(`Unable to save background completion status: ${error instanceof Error ? error.message : String(error)}`));
		return tasks;
	}

	async shutdown(): Promise<void> {
		if (!this.releaseLock && !this.initialized) return;
		if (this.shutdownPromise) return this.shutdownPromise;
		this.shutdownPromise = this.finishShutdown();
		return this.shutdownPromise;
	}

	private async finishShutdown(): Promise<void> {
		this.closed = true;
		if (!this.releaseLock) return;
		const active = [...this.running.entries()];
		for (const [id, running] of active) {
			const task = this.tasks.get(id);
			if (task?.state === "running") {
				task.state = "cancelled";
				task.finishedAt = Date.now();
			}
			running.controller.abort();
		}
		await Promise.all(active.map(([, running]) => running.processDone));
		if (this.persistTimer) clearTimeout(this.persistTimer);
		try {
			if (!this.compromised) await this.persist();
		} finally {
			if (!this.compromised) await this.releaseLock?.();
			this.releaseLock = undefined;
		}
	}

	private async run(task: BackgroundTask, controller: AbortController, timeoutSeconds?: number): Promise<void> {
		const onData = (chunk: Buffer) => {
			this.appendOutput(task, chunk);
			this.changed();
			this.schedulePersist();
		};
		let state: BackgroundTaskState = "failed";
		let exitCode: number | null | undefined;
		try {
			const result = await this.options.operations.exec(task.command, task.cwd, {
				onData,
				signal: controller.signal,
				timeout: timeoutSeconds,
			});
			exitCode = result.exitCode;
			state = exitCode === 0 ? "succeeded" : "failed";
		} catch (error) {
			const message = error instanceof Error ? error.message : String(error);
			state = task.state === "cancelled" || controller.signal.aborted ? "cancelled" : message.startsWith("timeout:") ? "timed_out" : "failed";
			exitCode = null;
			if (state === "failed") onData(Buffer.from(`${message}\n`));
		} finally {
			if (this.compromised) {
				task.state = "interrupted";
				task.exitCode = null;
				task.finishedAt = task.finishedAt ?? Date.now();
				this.running.delete(task.id);
				this.changed();
				return;
			}
			if (!isTerminal(task.state)) task.state = state;
			task.exitCode = exitCode;
			task.finishedAt = task.finishedAt ?? Date.now();
			this.completions.push(task.id);
			this.changed();
			try {
				await this.persist();
			} catch (error) {
				this.failForPersistence(task, error);
				this.changed();
			}
			this.running.delete(task.id);
			if (task.state !== "cancelled") void Promise.resolve(this.options.onTerminal?.(this.publicTask(task))).catch((error: unknown) => process.emitWarning(`Background completion notification failed: ${error instanceof Error ? error.message : String(error)}`));
		}
	}

	private appendOutput(task: BackgroundTask, chunk: Buffer): void {
		if (chunk.length === 0) return;
		const existing = Buffer.from(task.output, "base64");
		const joined = Buffer.concat([existing, chunk]);
		const excess = Math.max(0, joined.length - BACKGROUND_MAX_OUTPUT_BYTES);
		const retained = joined.subarray(excess);
		const alignedStart = utf8Boundary(retained, 0);
		task.outputBaseByte += excess + alignedStart;
		task.totalOutputBytes += chunk.length;
		task.output = retained.subarray(alignedStart).toString("base64");
	}

	private schedulePersist(): void {
		if (this.persistTimer || this.closed) return;
		this.persistTimer = setTimeout(() => {
			this.persistTimer = undefined;
			void this.persist().catch((error: unknown) => {
				for (const [id, running] of this.running) {
					const task = this.tasks.get(id);
					if (task) this.failForPersistence(task, error);
					running.controller.abort();
				}
				this.changed();
			});
		}, 150);
		this.persistTimer.unref();
	}

	private async persist(): Promise<void> {
		if (this.compromised) throw new Error("Background command session ownership was lost");
		const state: StoredState = { version: 1, tasks: [...this.tasks.values()].map((task) => ({ ...task })) };
		const json = JSON.stringify(state);
		this.writeChain = this.writeChain.catch(() => {}).then(async () => {
			if (this.compromised) throw new Error("Background command session ownership was lost");
			await mkdir(this.options.storageDirectory, { recursive: true, mode: 0o700 });
			const tempPath = `${this.statePath()}.${randomUUID()}.tmp`;
			await writeFile(tempPath, json, { mode: 0o600 });
			await chmod(tempPath, 0o600);
			await rename(tempPath, this.statePath());
		});
		await this.writeChain;
	}

	private statePath(): string {
		return join(this.options.storageDirectory, "tasks.json");
	}

	private requireTask(taskId: string): BackgroundTask {
		const exact = this.tasks.get(taskId);
		if (exact) return exact;
		const matches = [...this.tasks.values()].filter((task) => task.id.startsWith(taskId));
		if (matches.length === 1) return matches[0]!;
		if (matches.length > 1) throw new Error(`taskId prefix is ambiguous: ${taskId}`);
		throw new Error(`Unknown background task: ${taskId}`);
	}

	private validTask(task: BackgroundTask): boolean {
		return typeof task?.id === "string" && typeof task.command === "string" && typeof task.cwd === "string" &&
			["running", "succeeded", "failed", "cancelled", "timed_out", "interrupted"].includes(task.state) &&
			typeof task.output === "string" && Number.isSafeInteger(task.totalOutputBytes) && Number.isSafeInteger(task.outputBaseByte);
	}

	private publicTask(task: BackgroundTask): BackgroundTaskSummary {
		const { output: _output, outputBaseByte: _outputBaseByte, completionDelivered: _completionDelivered, persistenceErrorReported: _persistenceErrorReported, ...summary } = task;
		return { ...summary };
	}

	private failForPersistence(task: BackgroundTask, error: unknown): void {
		if (task.state === "cancelled") return;
		task.state = "failed";
		task.exitCode = null;
		task.finishedAt = task.finishedAt ?? Date.now();
		if (task.persistenceErrorReported) return;
		task.persistenceErrorReported = true;
		this.appendOutput(task, Buffer.from(`Unable to save background command state: ${error instanceof Error ? error.message : String(error)}\n`));
	}

	private changed(): void {
		this.options.onChange?.([...this.tasks.values()].map((task) => this.publicTask(task)));
	}

	private pruneTasks(): void {
		const terminal = [...this.tasks.values()].filter((task) => isTerminal(task.state) && task.completionDelivered).sort((a, b) => a.createdAt - b.createdAt);
		while (this.tasks.size >= BACKGROUND_MAX_TASKS && terminal.length > 0) this.tasks.delete(terminal.shift()!.id);
	}

	private ensureReady(): void {
		if (!this.initialized) throw new Error("Background command manager has not been initialized");
	}

	private onLockCompromised(error: Error): void {
		this.compromised = true;
		this.closed = true;
		for (const running of this.running.values()) running.controller.abort();
		process.emitWarning(`Background command session ownership was lost: ${error.message}`);
	}
}

function isMissingFile(error: unknown): boolean {
	return typeof error === "object" && error !== null && "code" in error && error.code === "ENOENT";
}

function isLocked(error: unknown): boolean {
	return typeof error === "object" && error !== null && "code" in error && error.code === "ELOCKED";
}
