import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

import { LOCATION_ENTRY_TYPE } from "./runtime.ts";

export interface Location {
	cwd: string;
	remote: string | null;
	branch: string | null;
	head: string | null;
}

const GIT_TIMEOUT_MS = 5_000;
const HTTP_USERINFO = /^(https?:\/\/)[^/@]*@/i;

export function sanitizeRemote(remote: string | null): string | null {
	if (remote === null) return null;
	return remote.replace(HTTP_USERINFO, "$1");
}

async function git(
	pi: ExtensionAPI,
	cwd: string,
	args: string[],
): Promise<string | null> {
	try {
		const result = await pi.exec("git", args, { cwd, timeout: GIT_TIMEOUT_MS });
		if (result.code !== 0) return null;
		const value = result.stdout.trim();
		return value.length > 0 ? value : null;
	} catch {
		return null;
	}
}

export async function readLocation(pi: ExtensionAPI, cwd: string): Promise<Location> {
	const [remote, branch, head] = await Promise.all([
		git(pi, cwd, ["config", "--get", "remote.origin.url"]),
		git(pi, cwd, ["rev-parse", "--abbrev-ref", "HEAD"]),
		git(pi, cwd, ["rev-parse", "HEAD"]),
	]);
	return { cwd, remote: sanitizeRemote(remote), branch, head };
}

export function sameLocation(left: Location | undefined, right: Location): boolean {
	return (
		left !== undefined &&
		left.cwd === right.cwd &&
		left.remote === right.remote &&
		left.branch === right.branch &&
		left.head === right.head
	);
}

export class LocationTracker {
	private last: Location | undefined;
	private readonly pi: ExtensionAPI;

	constructor(pi: ExtensionAPI) {
		this.pi = pi;
	}

	reset(): void {
		this.last = undefined;
	}

	async record(cwd: string): Promise<void> {
		const location = await readLocation(this.pi, cwd);
		if (sameLocation(this.last, location)) return;
		this.last = location;
		this.pi.appendEntry(LOCATION_ENTRY_TYPE, location);
	}
}
