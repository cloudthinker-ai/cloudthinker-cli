import {
	access,
	mkdir,
	mkdtemp,
	readFile,
	readdir,
	rename,
	rm,
	stat,
	utimes,
	writeFile,
} from "node:fs/promises";
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
const MAX_STAGE_AGE_MS = 24 * 60 * 60 * 1000;

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
	previousRevision?: string,
): Promise<void> {
	const target = skillTarget(root, name);
	const files = skillFiles(archive, root, name);
	await mkdir(root, { recursive: true });
	const staging = await mkdtemp(join(root, `.skill-stage-${process.pid}-`));
	const stagedSkill = join(staging, "new");
	const backup = join(staging, "old");
	const heartbeat = setInterval(() => {
		const now = new Date();
		void utimes(staging, now, now).catch(() => {});
	}, 60_000);
	heartbeat.unref();
	let movedOld = false;
	let preserveStaging = false;
	try {
		await writeFile(join(staging, "target.json"), JSON.stringify(name));
		await writeFile(join(staging, "revision.json"), JSON.stringify(previousRevision ?? null));
		const staged = [...files].map(
			([path, bytes]) => [join(stagedSkill, relative(target, path)), bytes] as const,
		);
		const directories = new Set(staged.map(([stagedPath]) => dirname(stagedPath)));
		await Promise.all([...directories].map((directory) => mkdir(directory, { recursive: true })));
		await Promise.all(staged.map(([stagedPath, bytes]) => writeFile(stagedPath, bytes)));
		try {
			await rename(target, backup);
			movedOld = true;
		} catch (error) {
			if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
		}
		try {
			await rename(stagedSkill, target);
		} catch (error) {
			if (movedOld) {
				try {
					await rename(backup, target);
					movedOld = false;
				} catch {
					preserveStaging = true;
				}
			}
			throw error;
		}
		if (movedOld) await rm(backup, { recursive: true, force: true }).catch(() => {});
	} finally {
		clearInterval(heartbeat);
		if (!preserveStaging) await rm(staging, { recursive: true, force: true }).catch(() => {});
	}
}

async function hasSkillFile(root: string, name: string): Promise<boolean> {
	try {
		return (await stat(join(skillTarget(root, name), "SKILL.md"))).isFile();
	} catch {
		return false;
	}
}

async function recoverAbandonedStages(root: string, indexPath: string): Promise<SkillIndex> {
	const recovered: SkillIndex = {};
	let entries;
	try {
		entries = await readdir(root, { withFileTypes: true });
	} catch {
		return recovered;
	}
	for (const entry of entries) {
		const match = /^\.skill-stage-(\d+)-([a-zA-Z0-9-]+)$/.exec(entry.name);
		if (!entry.isDirectory() || !match) continue;
		const ownerPid = Number(match[1]);
		const stage = join(root, entry.name);
		let ownerAlive = true;
		try {
			process.kill(ownerPid, 0);
		} catch (error) {
			ownerAlive = (error as NodeJS.ErrnoException).code !== "ESRCH";
		}
		if (ownerAlive) {
			const stageInfo = await stat(stage).catch(() => undefined);
			if (!stageInfo || Date.now() - stageInfo.mtimeMs <= MAX_STAGE_AGE_MS) continue;
		}
		const recoveryStage = join(root, `.skill-stage-${process.pid}-recover-${match[1]}-${match[2]}`);
		try {
			await rename(stage, recoveryStage);
		} catch {
			continue;
		}
		const backup = join(recoveryStage, "old");
		const stagedSkill = join(recoveryStage, "new");
		const hasBackup = await exists(backup);
		const targetName = await readJson(join(recoveryStage, "target.json"));
		if (typeof targetName !== "string") {
			if (!hasBackup) await rm(recoveryStage, { recursive: true, force: true }).catch(() => {});
			continue;
		}
		let target: string;
		try {
			target = skillTarget(root, targetName);
		} catch {
			if (!hasBackup) await rm(recoveryStage, { recursive: true, force: true }).catch(() => {});
			continue;
		}
		let preserveStage = false;
		if (hasBackup) {
			const targetExists = await exists(target);
			if (!targetExists) {
				const revision = await readJson(join(recoveryStage, "revision.json"));
				try {
					if (typeof revision === "string") {
						await writeIndex(indexPath, {
							...(await readIndex(indexPath)),
							[targetName]: revision,
						});
					}
				} catch (error) {
					await rename(recoveryStage, stage).catch(() => {});
					throw error;
				}
				try {
					await rename(backup, target);
					if (typeof revision === "string") recovered[targetName] = revision;
				} catch {
					preserveStage = true;
				}
			} else {
				const hasNew = await exists(stagedSkill);
				const hasActiveSkill = await hasSkillFile(root, targetName);
				preserveStage = hasNew || !hasActiveSkill;
			}
		}
		if (!preserveStage) await rm(recoveryStage, { recursive: true, force: true }).catch(() => {});
	}
	return recovered;
}

async function exists(path: string): Promise<boolean> {
	try {
		await access(path);
		return true;
	} catch {
		return false;
	}
}

async function readJson(path: string): Promise<unknown> {
	try {
		return JSON.parse(await readFile(path, "utf8")) as unknown;
	} catch {
		return undefined;
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
		return entries
			.filter((entry) => entry.isDirectory() && !entry.name.startsWith("."))
			.map((entry) => entry.name);
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
	const recovered = await recoverAbandonedStages(root, indexPath);
	const listed: WorkspaceSkill[] = await client.listSkills();
	const installable = listed.filter(
		(skill) => skill.enabled && skill.content_status === "available",
	);
	const cached = { ...(await readIndex(indexPath)), ...recovered };
	const onDisk = new Set(await cachedNames(root));
	const next: SkillIndex = {};
	const installed: string[] = [];
	const failed = new Set<string>();
	const pending: WorkspaceSkill[] = [];
	for (const skill of installable) {
		if (cached[skill.name] === skill.updated_at && onDisk.has(skill.name)) {
			next[skill.name] = skill.updated_at;
			continue;
		}
		pending.push(skill);
	}
	const INSTALL_BATCH = 4;
	for (let start = 0; start < pending.length; start += INSTALL_BATCH) {
		const results = await Promise.all(
			pending.slice(start, start + INSTALL_BATCH).map(async (skill) => {
				try {
					skillTarget(root, skill.name);
					await unpackSkill(
						await client.downloadSkill(skill.name),
						root,
						skill.name,
						cached[skill.name],
					);
					return { skill, ok: true as const };
				} catch (error) {
					return { skill, ok: false as const, error };
				}
			}),
		);
		for (const result of results) {
			const { skill } = result;
			if (result.ok) {
				next[skill.name] = skill.updated_at;
				installed.push(skill.name);
				continue;
			}
			failed.add(skill.name);
			const cachedRevision = cached[skill.name];
			if (
				onDisk.has(skill.name) &&
				cachedRevision !== undefined &&
				(await hasSkillFile(root, skill.name))
			) {
				next[skill.name] = cachedRevision;
			}
			onSkillError(skill.name, result.error);
		}
	}
	const removed: string[] = [];
	for (const name of onDisk) {
		if (name in next || failed.has(name)) continue;
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
