import { CloudThinkerApiError } from "./client.ts";

export const RETRY_DELAYS_MS: readonly number[] = [5_000, 10_000, 30_000, 60_000];
export const RETRY_COMMAND = "/cloud retry";

export type CloudFailureKind = "signed-out" | "denied" | "offline" | "server" | "rejected";

export interface CloudFailure {
	kind: CloudFailureKind;
	label: string;
	cause: string;
	action: string;
	detail: string;
	retry: boolean;
}

function message(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}

export function classifyCloudError(error: unknown): CloudFailure {
	const detail = message(error);
	const status = error instanceof CloudThinkerApiError ? error.status : undefined;
	if (status === 401) {
		return { kind: "signed-out", label: "signed out", cause: "You are signed out of CloudThinker", action: `run \`cloudthinker login\` in a terminal, then ${RETRY_COMMAND}`, detail, retry: false };
	}
	if (status === 403) {
		return { kind: "denied", label: "access denied", cause: "CloudThinker denied access to this workspace", action: `check this workspace with \`cloudthinker whoami\`, then ${RETRY_COMMAND}`, detail, retry: false };
	}
	if (status === 0) {
		const origin = /^Could not reach (\S+): /.exec(detail)?.[1];
		const host = origin ? origin.replace(/^https?:\/\//, "") : "CloudThinker";
		return { kind: "offline", label: `offline: can't reach ${host}`, cause: `CloudThinker can't reach ${host}`, action: RETRY_COMMAND, detail, retry: true };
	}
	if (status !== undefined && (status === 408 || status === 429 || status >= 500)) {
		return { kind: "server", label: `server error ${status}`, cause: `CloudThinker answered with server error ${status}`, action: RETRY_COMMAND, detail, retry: true };
	}
	return { kind: "rejected", label: "cloud unavailable", cause: "CloudThinker could not open this session", action: `${RETRY_COMMAND}, or /cloud for details`, detail, retry: false };
}

export function retryDelay(attempt: number): number {
	return RETRY_DELAYS_MS[Math.min(attempt, RETRY_DELAYS_MS.length - 1)]!;
}

export function failureStatus(failure: CloudFailure, delayMs: number | undefined): string {
	if (delayMs === undefined) return failure.action;
	return `retrying in ${Math.round(delayMs / 1000)}s · ${RETRY_COMMAND} to try now`;
}

export function failureNotice(failure: CloudFailure): string {
	const action = failure.retry ? `Retrying automatically; ${RETRY_COMMAND} tries now.` : `Next: ${failure.action}.`;
	return `${failure.cause}, so cloud tools and the model gateway are unavailable. ${action}\n${failure.detail}`;
}
