import type { BashOperations } from "@earendil-works/pi-coding-agent";

import { BACKGROUND_MAX_OUTPUT_BYTES, type AdoptedCommand, type BackgroundTaskSummary } from "./manager.ts";

export const AUTO_BACKGROUND_ENV = "CLOUDTHINKER_AUTO_BACKGROUND_SECONDS";
export const AUTO_BACKGROUND_DEFAULT_SECONDS = 10;

type ExecOptions = Parameters<BashOperations["exec"]>[2];

export function autoBackgroundSeconds(env: NodeJS.ProcessEnv = process.env): number {
	const raw = env[AUTO_BACKGROUND_ENV]?.trim();
	if (!raw) return AUTO_BACKGROUND_DEFAULT_SECONDS;
	const seconds = Number(raw);
	return Number.isFinite(seconds) && seconds >= 0 ? seconds : AUTO_BACKGROUND_DEFAULT_SECONDS;
}

export function staysInForeground(command: string): boolean {
	return /^\s*sleep\b/.test(command);
}

export class ForegroundShells {
	private readonly moves = new Set<() => void>();

	get size(): number {
		return this.moves.size;
	}

	add(move: () => void): () => void {
		this.moves.add(move);
		return () => this.moves.delete(move);
	}

	moveAll(): number {
		const moves = [...this.moves];
		for (const move of moves) move();
		return moves.length;
	}
}

class Backlog {
	private chunks: Buffer[] = [];
	private size = 0;
	droppedBytes = 0;

	push(chunk: Buffer): void {
		this.chunks.push(chunk);
		this.size += chunk.length;
		while (this.size > BACKGROUND_MAX_OUTPUT_BYTES && this.chunks.length > 1) {
			const dropped = this.chunks.shift()!;
			this.size -= dropped.length;
			this.droppedBytes += dropped.length;
		}
	}

	take(): Buffer {
		const joined = Buffer.concat(this.chunks);
		this.chunks = [];
		this.size = 0;
		return joined;
	}
}

export interface MovableShellOptions {
	local: BashOperations;
	displayCommand: string;
	seconds: number;
	foreground: ForegroundShells;
	adopt: (command: AdoptedCommand) => Promise<BackgroundTaskSummary>;
	onMoved: (task: BackgroundTaskSummary) => void;
}

export function movableShell(options: MovableShellOptions): BashOperations {
	return {
		exec: (command, cwd, execOptions) => runOrMove(command, cwd, execOptions, options),
	};
}

async function runOrMove(command: string, cwd: string, execOptions: ExecOptions, options: MovableShellOptions): Promise<{ exitCode: number | null }> {
	const controller = new AbortController();
	const abort = () => controller.abort();
	const callerSignal = execOptions.signal;
	const listen = () => {
		if (callerSignal?.aborted) abort();
		else callerSignal?.addEventListener("abort", abort, { once: true });
	};
	listen();
	const backlog = new Backlog();
	const startedAt = Date.now();
	let adopted: ((chunk: Buffer) => void) | undefined;
	const done = options.local.exec(command, cwd, {
		...execOptions,
		signal: controller.signal,
		onData: (chunk) => {
			if (adopted) return adopted(chunk);
			backlog.push(chunk);
			execOptions.onData(chunk);
		},
	});
	let requestMove!: () => void;
	const moveRequested = new Promise<"move">((resolve) => { requestMove = () => resolve("move"); });
	const timer = options.seconds > 0 ? setTimeout(requestMove, options.seconds * 1000) : undefined;
	const unregister = options.foreground.add(requestMove);
	try {
		const winner = await Promise.race([done, moveRequested]);
		if (winner !== "move" || controller.signal.aborted) return await done;
		let task: BackgroundTaskSummary | undefined;
		try {
			task = await options.adopt({
				command: options.displayCommand,
				cwd,
				startedAt,
				done,
				abort,
				attach: (onData) => {
					adopted = onData;
					return { backlog: backlog.take(), droppedBytes: backlog.droppedBytes };
				},
			});
		} catch {
			task = undefined;
		}
		if (task?.state !== "running" || controller.signal.aborted) {
			unregister();
			return await done;
		}
		callerSignal?.removeEventListener("abort", abort);
		options.onMoved(task);
		return { exitCode: 0 };
	} finally {
		if (timer) clearTimeout(timer);
		unregister();
		callerSignal?.removeEventListener("abort", abort);
	}
}
