import { access, mkdir, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { dirname, join, relative, resolve, sep } from "node:path";

import { unzipSync } from "fflate";

import type { CloudThinkerClient, WorkspaceSkill } from "./client.ts";
import { skillsIndexPath, skillsRoot } from "./paths.ts";

export type SkillIndex = Record<string, string>;

export interface SkillRefresh {
	installed: string[];
	removed: string[];
}

export class SkillZipError extends Error {}

const SKILL_NAME = /^[A-Za-z0-9_-][A-Za-z0-9._-]*$/;

function isInside(root: string, candidate: string): boolean {
	const rel = relative(root, candidate);
	return rel.length > 0 && !rel.startsWith("..") && !rel.startsWith(sep);
}

export function skillTarget(root: string, name: string): string {
	const target = resolve(root, name);
	if (!SKILL_NAME.test(name) || !isInside(root, target)) {
		throw new SkillZipError(`Skill name "${name}" is not a plain identifier`);
	}
	return target;
}

export function skillFiles(
	archive: Uint8Array,
	root: string,
	name: string,
): Map<string, Uint8Array> {
	const target = skillTarget(root, name);
	const files = new Map<string, Uint8Array>();
	for (const [entry, bytes] of Object.entries(unzipSync(archive))) {
		if (entry.endsWith("/")) continue;
		const destination = resolve(root, entry);
		if (!isInside(target, destination)) {
			throw new SkillZipError(
				`Skill "${name}" contains an entry outside its own directory: ${entry}`,
			);
		}
		files.set(destination, bytes);
	}
	if (!files.has(join(target, "SKILL.md"))) {
		throw new SkillZipError(`Skill "${name}" has no SKILL.md`);
	}
	return files;
}

export async function unpackSkill(
	archive: Uint8Array,
	root: string,
	name: string,
): Promise<void> {
	const files = skillFiles(archive, root, name);
	await rm(skillTarget(root, name), { recursive: true, force: true });
	for (const [path, bytes] of files) {
		await mkdir(dirname(path), { recursive: true });
		await writeFile(path, bytes);
	}
}

export async function readIndex(path: string): Promise<SkillIndex> {
	try {
		const parsed: unknown = JSON.parse(await readFile(path, "utf8"));
		return typeof parsed === "object" && parsed !== null ? (parsed as SkillIndex) : {};
	} catch {
		return {};
	}
}

async function writeIndex(path: string, index: SkillIndex): Promise<void> {
	await mkdir(dirname(path), { recursive: true });
	await writeFile(path, `${JSON.stringify(index, null, 2)}\n`, "utf8");
}

async function cachedNames(root: string): Promise<string[]> {
	try {
		const entries = await readdir(root, { withFileTypes: true });
		return entries.filter((entry) => entry.isDirectory()).map((entry) => entry.name);
	} catch {
		return [];
	}
}

export async function refreshSkills(
	client: CloudThinkerClient,
	workspaceId: string,
	agentDir?: string,
	onSkillError: (name: string, error: unknown) => void = () => {},
): Promise<SkillRefresh> {
	const root = skillsRoot(workspaceId, agentDir);
	const indexPath = skillsIndexPath(workspaceId, agentDir);
	const listed: WorkspaceSkill[] = await client.listSkills();
	const installable = listed.filter(
		(skill) => skill.enabled && skill.content_status === "available",
	);
	const cached = await readIndex(indexPath);
	const onDisk = new Set(await cachedNames(root));
	const next: SkillIndex = {};
	const installed: string[] = [];
	for (const skill of installable) {
		if (cached[skill.name] === skill.updated_at && onDisk.has(skill.name)) {
			next[skill.name] = skill.updated_at;
			continue;
		}
		try {
			skillTarget(root, skill.name);
			await unpackSkill(await client.downloadSkill(skill.name), root, skill.name);
			next[skill.name] = skill.updated_at;
			installed.push(skill.name);
		} catch (error) {
			onSkillError(skill.name, error);
		}
	}
	const removed: string[] = [];
	for (const name of onDisk) {
		if (name in next) continue;
		await rm(join(root, name), { recursive: true, force: true });
		removed.push(name);
	}
	await writeIndex(indexPath, next);
	return { installed, removed };
}

export async function hasSkillIndex(
	workspaceId: string,
	agentDir?: string,
): Promise<boolean> {
	try {
		await access(skillsIndexPath(workspaceId, agentDir));
		return true;
	} catch {
		return false;
	}
}

export async function discoverSkillPaths(
	workspaceId: string | undefined,
	agentDir?: string,
): Promise<string[]> {
	if (!workspaceId) return [];
	const root = skillsRoot(workspaceId, agentDir);
	return (await cachedNames(root)).length > 0 ? [root] : [];
}
