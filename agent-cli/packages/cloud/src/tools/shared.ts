import type { AgentToolResult } from "@earendil-works/pi-coding-agent";

import { CloudThinkerApiError } from "../client.ts";
import type { CloudThinkerRuntime } from "../runtime.ts";

export function text<T>(body: string, details: T): AgentToolResult<T> {
	return { content: [{ type: "text", text: body }], details };
}

export function section(label: string, body: string): string {
	const trimmed = body.trimEnd();
	return `${label}:\n${trimmed.length > 0 ? trimmed : "(empty)"}`;
}

export function sleep(ms: number, signal?: AbortSignal): Promise<void> {
	return new Promise((resolve, reject) => {
		if (signal?.aborted) {
			reject(new Error("Cancelled"));
			return;
		}
		const timer = setTimeout(() => {
			signal?.removeEventListener("abort", onAbort);
			resolve();
		}, ms);
		const onAbort = (): void => {
			clearTimeout(timer);
			reject(new Error("Cancelled"));
		};
		signal?.addEventListener("abort", onAbort, { once: true });
	});
}

export interface PollUntilOptions<T> {
	read: (signal?: AbortSignal) => Promise<T>;
	settled: (value: T) => boolean;
	intervalMs: number;
	maxWaitMs: number;
	onTick?: (value: T, elapsedMs: number) => void;
	signal?: AbortSignal;
	now?: () => number;
}

export async function pollUntil<T>(options: PollUntilOptions<T>): Promise<T> {
	const now = options.now ?? Date.now;
	const deadline = now() + options.maxWaitMs;
	const started = now();
	for (;;) {
		const value = await options.read(options.signal);
		options.onTick?.(value, now() - started);
		if (options.settled(value)) return value;
		if (now() >= deadline) return value;
		await sleep(options.intervalMs, options.signal);
	}
}

export function explain(
	error: unknown,
	requested: string[],
	runtime: Pick<CloudThinkerRuntime, "connectedPrefixes">,
): string {
	if (error instanceof CloudThinkerApiError && error.status === 422) {
		const connected = runtime.connectedPrefixes;
		const unknown = requested.filter((prefix) => !connected.includes(prefix));
		const named = unknown.length > 0 ? unknown : requested;
		return [
			`${error.message}`,
			`Requested: ${requested.join(", ") || "(none)"}.`,
			`Not connected in this workspace: ${named.join(", ") || "(none)"}.`,
			`Connected prefixes: ${connected.join(", ") || "(none)"}.`,
			"Use one of the connected prefixes, or ask the user to connect the provider in CloudThinker.",
		].join(" ");
	}
	return error instanceof Error ? error.message : String(error);
}
