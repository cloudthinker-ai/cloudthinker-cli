import { readFileSync } from "node:fs";
import { join } from "node:path";

import { VERSION, getPackageDir } from "@earendil-works/pi-coding-agent";

export const PI_AUTHOR = "Mario Zechner";
export const PI_LICENSE = "MIT";

export interface HostVersions {
	host: string;
	pi: string;
	piRepositoryUrl: string;
}

interface SidecarPackage {
	version?: unknown;
	piVersion?: unknown;
	piRepository?: unknown;
	repository?: { url?: unknown } | unknown;
}

function str(value: unknown): string {
	return typeof value === "string" ? value : "";
}

export function normalizeRepositoryUrl(url: string): string {
	return url.replace(/^git\+/, "").replace(/\.git$/, "");
}

export function hostVersionsFrom(pkg: SidecarPackage, host: string): HostVersions {
	const piVersion = str(pkg.piVersion);
	const repository = pkg.repository;
	const declared = str(pkg.piRepository);
	const own =
		typeof repository === "object" && repository !== null
			? str((repository as { url?: unknown }).url)
			: str(repository);
	return {
		host,
		pi: piVersion.length > 0 ? piVersion : str(pkg.version),
		piRepositoryUrl: normalizeRepositoryUrl(declared.length > 0 ? declared : own),
	};
}

export function readHostVersions(packageDir: string = getPackageDir()): HostVersions {
	let pkg: SidecarPackage = {};
	try {
		pkg = JSON.parse(readFileSync(join(packageDir, "package.json"), "utf8")) as SidecarPackage;
	} catch {
		pkg = {};
	}
	return hostVersionsFrom(pkg, VERSION);
}

export function attributionLine(versions: HostVersions): string {
	const url = versions.piRepositoryUrl;
	return `built on pi by ${PI_AUTHOR}, ${PI_LICENSE}${url.length > 0 ? `, ${url}` : ""}`;
}
