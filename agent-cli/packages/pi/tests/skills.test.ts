import assert from "node:assert/strict";
import { mkdir, readFile, readdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import test from "node:test";

import { zipSync } from "fflate";

import { CloudThinkerClient, TokenSource, type WorkspaceSkill } from "../src/client.ts";
import { skillsIndexPath, skillsRoot } from "../src/paths.ts";
import {
	SkillZipError,
	discoverSkillPaths,
	hasSkillIndex,
	readIndex,
	refreshSkills,
	skillFiles,
} from "../src/skills.ts";
import { startFakeServer, withTempDir } from "./helpers.ts";

const WORKSPACE = "22222222-2222-2222-2222-222222222222";
const encoder = new TextEncoder();

function skillZip(name: string, body: string, extra: Record<string, string> = {}) {
	const files: Record<string, Uint8Array> = {
		[`${name}/SKILL.md`]: encoder.encode(body),
	};
	for (const [path, content] of Object.entries(extra)) {
		files[path] = encoder.encode(content);
	}
	return zipSync(files);
}

function listing(overrides: Partial<WorkspaceSkill>[]): WorkspaceSkill[] {
	return overrides.map((override) => ({
		name: "alpha",
		description: "d",
		enabled: true,
		updated_at: "2026-09-01T00:00:00Z",
		content_status: "available" as const,
		...override,
	}));
}

function clientFor(origin: string): CloudThinkerClient {
	return new CloudThinkerClient({
		baseUrl: origin,
		tokens: new TokenSource({ CLOUDTHINKER_TOKEN: "t" }),
	});
}

test("a zip entry outside the skill directory is rejected", () => {
	const escaping = zipSync({
		"alpha/SKILL.md": encoder.encode("ok"),
		"../evil.sh": encoder.encode("rm -rf /"),
	});
	assert.throws(() => skillFiles(escaping, "/tmp/root", "alpha"), SkillZipError);

	const sibling = zipSync({
		"alpha/SKILL.md": encoder.encode("ok"),
		"beta/SKILL.md": encoder.encode("other"),
	});
	assert.throws(() => skillFiles(sibling, "/tmp/root", "alpha"), SkillZipError);
});

test("a zip with no SKILL.md is rejected", () => {
	const missing = zipSync({ "alpha/notes.md": encoder.encode("x") });
	assert.throws(() => skillFiles(missing, "/tmp/root", "alpha"), SkillZipError);
});

test("refresh installs, re-downloads on a new stamp, and deletes what is gone", async () => {
	let skills = listing([{ name: "alpha" }, { name: "beta" }]);
	const bodies = new Map([
		["alpha", "alpha v1"],
		["beta", "beta v1"],
	]);
	const downloads: string[] = [];
	const server = await startFakeServer((request) => {
		if (request.path === "/api/v1/custom-skills/") return { body: skills };
		const match = /\/api\/v1\/custom-skills\/(.+)\/download$/.exec(request.path);
		if (match?.[1]) {
			downloads.push(match[1]);
			return {
				raw: Buffer.from(skillZip(match[1], bodies.get(match[1]) ?? "")),
			};
		}
		return undefined;
	});
	try {
		await withTempDir(async (dir) => {
			const client = clientFor(server.origin);
			const root = skillsRoot(WORKSPACE, dir);

			const first = await refreshSkills(client, WORKSPACE, dir);
			assert.deepEqual(first.installed.sort(), ["alpha", "beta"]);
			assert.equal(
				await readFile(join(root, "alpha", "SKILL.md"), "utf8"),
				"alpha v1",
			);
			assert.deepEqual(await discoverSkillPaths(WORKSPACE, dir), [root]);

			downloads.length = 0;
			const second = await refreshSkills(client, WORKSPACE, dir);
			assert.deepEqual(second.installed, []);
			assert.deepEqual(downloads, []);

			bodies.set("alpha", "alpha v2");
			skills = listing([
				{ name: "alpha", updated_at: "2026-09-05T00:00:00Z" },
				{ name: "beta" },
			]);
			downloads.length = 0;
			const third = await refreshSkills(client, WORKSPACE, dir);
			assert.deepEqual(third.installed, ["alpha"]);
			assert.deepEqual(downloads, ["alpha"]);
			assert.equal(
				await readFile(join(root, "alpha", "SKILL.md"), "utf8"),
				"alpha v2",
			);

			skills = listing([
				{ name: "alpha", updated_at: "2026-09-05T00:00:00Z" },
				{ name: "beta", enabled: false },
			]);
			const fourth = await refreshSkills(client, WORKSPACE, dir);
			assert.deepEqual(fourth.removed, ["beta"]);
			assert.deepEqual(await readdir(root), ["alpha"]);
			assert.deepEqual(Object.keys(await readIndex(skillsIndexPath(WORKSPACE, dir))), [
				"alpha",
			]);
		});
	} finally {
		await server.close();
	}
});

test("a rejected zip leaves the other skills installed and reports the name", async () => {
	const server = await startFakeServer((request) => {
		if (request.path === "/api/v1/custom-skills/") {
			return { body: listing([{ name: "alpha" }, { name: "evil" }]) };
		}
		if (request.path.includes("/evil/")) {
			return {
				raw: Buffer.from(
					zipSync({
						"evil/SKILL.md": encoder.encode("ok"),
						"../escape.md": encoder.encode("x"),
					}),
				),
			};
		}
		return { raw: Buffer.from(skillZip("alpha", "alpha")) };
	});
	try {
		await withTempDir(async (dir) => {
			const failures: string[] = [];
			const result = await refreshSkills(
				clientFor(server.origin),
				WORKSPACE,
				dir,
				(name) => failures.push(name),
			);
			assert.deepEqual(result.installed, ["alpha"]);
			assert.deepEqual(failures, ["evil"]);
			assert.deepEqual(await readdir(skillsRoot(WORKSPACE, dir)), ["alpha"]);
		});
	} finally {
		await server.close();
	}
});

test("a skill with no downloadable content is skipped without an error", async () => {
	let skills = listing([{ name: "alpha" }, { name: "beta" }]);
	const downloads: string[] = [];
	const server = await startFakeServer((request) => {
		if (request.path === "/api/v1/custom-skills/") return { body: skills };
		const match = /\/api\/v1\/custom-skills\/(.+)\/download$/.exec(request.path);
		if (match?.[1]) {
			downloads.push(match[1]);
			return { raw: Buffer.from(skillZip(match[1], match[1])) };
		}
		return undefined;
	});
	try {
		await withTempDir(async (dir) => {
			const client = clientFor(server.origin);
			const failures: string[] = [];

			skills = listing([
				{ name: "alpha" },
				{ name: "beta", content_status: "missing" },
			]);
			const first = await refreshSkills(client, WORKSPACE, dir, (name) =>
				failures.push(name),
			);
			assert.deepEqual(first.installed, ["alpha"]);
			assert.equal(failures.length, 0);
			assert.deepEqual(downloads, ["alpha"]);
			assert.deepEqual(await readdir(skillsRoot(WORKSPACE, dir)), ["alpha"]);

			skills = listing([{ name: "alpha" }, { name: "beta" }]);
			downloads.length = 0;
			const second = await refreshSkills(client, WORKSPACE, dir, (name) =>
				failures.push(name),
			);
			assert.deepEqual(second.installed, ["beta"]);
			assert.deepEqual(downloads, ["beta"]);
			assert.equal(failures.length, 0);
		});
	} finally {
		await server.close();
	}
});

test("a skill that loses its content is dropped from the cache", async () => {
	let skills = listing([{ name: "alpha" }]);
	const server = await startFakeServer((request) => {
		if (request.path === "/api/v1/custom-skills/") return { body: skills };
		return { raw: Buffer.from(skillZip("alpha", "alpha")) };
	});
	try {
		await withTempDir(async (dir) => {
			const client = clientFor(server.origin);
			await refreshSkills(client, WORKSPACE, dir);
			assert.deepEqual(await readdir(skillsRoot(WORKSPACE, dir)), ["alpha"]);

			skills = listing([{ name: "alpha", content_status: "missing" }]);
			const second = await refreshSkills(client, WORKSPACE, dir);
			assert.deepEqual(second.removed, ["alpha"]);
			assert.deepEqual(
				Object.keys(await readIndex(skillsIndexPath(WORKSPACE, dir))),
				[],
			);
		});
	} finally {
		await server.close();
	}
});

test("the cache is scoped per workspace and not under pi's own skills directory", async () => {
	await withTempDir(async (dir) => {
		const root = skillsRoot(WORKSPACE, dir);
		assert.ok(root.startsWith(join(dir, "cloudthinker", "skills")));
		assert.ok(!root.startsWith(join(dir, "skills")));
		assert.deepEqual(await discoverSkillPaths(WORKSPACE, dir), []);
		assert.deepEqual(await discoverSkillPaths(undefined, dir), []);
		await mkdir(join(root, "alpha"), { recursive: true });
		await writeFile(join(root, "alpha", "SKILL.md"), "x");
		assert.deepEqual(await discoverSkillPaths(WORKSPACE, dir), [root]);
	});
});

test("a skill name that is not a plain identifier never becomes a path", () => {
	for (const name of ["/home/x/.ssh", "../../.ssh", "..", "a/b", "a\\b", ".hidden", ""]) {
		assert.throws(() => skillFiles(skillZip(name, "ok"), "/tmp/root", name), SkillZipError, name);
	}
	assert.ok(skillFiles(skillZip("aws-cost_v2.1", "ok"), "/tmp/root", "aws-cost_v2.1").size === 1);
});

test("a listed skill with an escaping name is skipped before its download and the rest still refresh", async () => {
	const downloads: string[] = [];
	const server = await startFakeServer((request) => {
		if (request.path === "/api/v1/custom-skills/") {
			return { body: listing([{ name: "../../escape" }, { name: "alpha" }]) };
		}
		const match = /\/api\/v1\/custom-skills\/(.+)\/download$/.exec(request.path);
		if (match?.[1]) {
			downloads.push(decodeURIComponent(match[1]));
			return { raw: Buffer.from(skillZip("alpha", "alpha")) };
		}
		return undefined;
	});
	try {
		await withTempDir(async (dir) => {
			const failures: { name: string; error: unknown }[] = [];
			const result = await refreshSkills(clientFor(server.origin), WORKSPACE, dir, (name, error) =>
				failures.push({ name, error }),
			);
			assert.deepEqual(result.installed, ["alpha"]);
			assert.deepEqual(downloads, ["alpha"]);
			assert.equal(failures[0]?.name, "../../escape");
			assert.ok(failures[0]?.error instanceof SkillZipError);
			assert.deepEqual(await readdir(skillsRoot(WORKSPACE, dir)), ["alpha"]);
		});
	} finally {
		await server.close();
	}
});

test("a workspace id that is not a UUID never becomes a path segment", () => {
	assert.throws(() => skillsRoot("../etc", "/tmp/agent"));
	assert.throws(() => skillsIndexPath("w-1", "/tmp/agent"));
	assert.equal(skillsRoot(WORKSPACE, "/tmp/agent"), join("/tmp/agent", "cloudthinker", "skills", WORKSPACE));
});

test("a cold cache is distinguishable from a workspace whose skills are all gone", async () => {
	const server = await startFakeServer((request) =>
		request.path === "/api/v1/custom-skills/" ? { body: [] } : undefined,
	);
	try {
		await withTempDir(async (dir) => {
			assert.equal(await hasSkillIndex(WORKSPACE, dir), false);

			await refreshSkills(clientFor(server.origin), WORKSPACE, dir);

			assert.equal(await hasSkillIndex(WORKSPACE, dir), true);
			assert.deepEqual(await discoverSkillPaths(WORKSPACE, dir), []);
		});
	} finally {
		await server.close();
	}
});

test("an unprobed skill is not downloaded, so an executor outage is not a 404 storm", async () => {
	const downloads: string[] = [];
	const server = await startFakeServer((request) => {
		if (request.path === "/api/v1/custom-skills/") {
			return {
				body: listing([
					{ name: "alpha", content_status: "unknown" },
					{ name: "beta", content_status: "invalid" },
				]),
			};
		}
		const match = /\/api\/v1\/custom-skills\/(.+)\/download$/.exec(request.path);
		if (match?.[1]) {
			downloads.push(match[1]);
			return { status: 404, body: { detail: "Skill not found" } };
		}
		return undefined;
	});
	try {
		await withTempDir(async (dir) => {
			const errors: string[] = [];
			const result = await refreshSkills(
				clientFor(server.origin),
				WORKSPACE,
				dir,
				(name) => errors.push(name),
			);

			assert.deepEqual(downloads, []);
			assert.deepEqual(result.installed, []);
			assert.deepEqual(errors, []);
		});
	} finally {
		await server.close();
	}
});
