import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { DefaultResourceLoader, SessionManager, SettingsManager, createAgentSession, formatSkillsForPrompt, loadSkillsFromDir } from "@earendil-works/pi-coding-agent";
import { applySkillCatalogGuard, SKILL_CATALOG_BYTES } from "../src/skill-catalog.ts";

test("SDK sessions bound automatic skill metadata without losing discovery, reload, or explicit invocation", async () => {
	const root = mkdtempSync(join(tmpdir(), "ct-skill-catalog-"));
	const previous = process.env.PI_CODING_AGENT_DIR;
	process.env.PI_CODING_AGENT_DIR = root;
	applySkillCatalogGuard();
	let session: Awaited<ReturnType<typeof createAgentSession>>["session"] | undefined;
	try {
		for (let index = 0; index < 100; index++) {
			const name = `catalog-${String(index).padStart(3, "0")}`;
			const directory = join(root, "skills", name);
			mkdirSync(directory, { recursive: true });
			writeFileSync(join(directory, "SKILL.md"), `---\nname: ${name}\ndescription: ${"Catalog fixture & metadata ".repeat(12)}\n---\n\nBody marker ${index}.\n`);
		}
		const hidden = join(root, "skills", "manual-only");
		mkdirSync(hidden);
		writeFileSync(join(hidden, "SKILL.md"), "---\nname: manual-only\ndescription: Private manual metadata\ndisable-model-invocation: true\n---\n\nManual body.\n");
		const settingsManager = SettingsManager.inMemory({});
		const loader = new DefaultResourceLoader({ cwd: root, agentDir: root, settingsManager, noSkills: true, skillsOverride: () => existsSync(join(root, "skills")) ? loadSkillsFromDir({ dir: join(root, "skills"), source: "user" }) : { skills: [], diagnostics: [] }, noExtensions: true, noThemes: true, noPromptTemplates: true, systemPrompt: "CUSTOM CONTRACT" });
		await loader.reload();
		session = (await createAgentSession({ cwd: root, agentDir: root, settingsManager, resourceLoader: loader, sessionManager: SessionManager.inMemory(root) })).session;
		assert.equal(session.resourceLoader.getSkills().skills.length, 101);
		assert.ok(session.systemPrompt.startsWith("CUSTOM CONTRACT"));
		assert.ok(!session.systemPrompt.includes("Private manual metadata"));
		const guidanceStart = session.systemPrompt.indexOf("\n\nSkill catalog:");
		assert.ok(guidanceStart > 0);
		const guidance = session.systemPrompt.slice(guidanceStart);
		const location = JSON.parse(guidance.match(/catalog is at (".*?") \(JSONL/)![1]!);
		const rows = readFileSync(location, "utf8").trim().split("\n").map((line) => JSON.parse(line));
		assert.equal(rows.length, 100);
		assert.equal(statSync(location).mode & 0o777, 0o600);
		const selected = loader.getSkills().skills.filter((skill) => session!.systemPrompt.includes(`<name>${skill.name}</name>`));
		assert.ok(selected.length > 0 && selected.length < 100);
		assert.ok(Buffer.byteLength(formatSkillsForPrompt(selected, "read") + guidance) <= SKILL_CATALOG_BYTES);
		const omitted = rows.find((row) => !selected.some((skill) => skill.name === row.name));
		assert.ok(readFileSync(omitted.location, "utf8").includes("Body marker"));
		const expanded = (session as unknown as { _expandSkillCommand(text: string): string })._expandSkillCommand(`/skill:${omitted.name} use this`);
		assert.ok(expanded.includes("Body marker"));
		session.setActiveToolsByName([]);
		assert.ok(!session.systemPrompt.includes("Skill catalog:"));
		assert.ok(!session.systemPrompt.includes("<available_skills>"));
		rmSync(join(root, "skills"), { recursive: true });
		await loader.reload();
		session.setActiveToolsByName(["read"]);
		assert.ok(!session.systemPrompt.includes("Skill catalog:"));
		assert.ok(existsSync(location));
		assert.equal(session.resourceLoader.getSkills().skills.length, 0);
		const small = join(root, "skills", "small");
		mkdirSync(small, { recursive: true });
		writeFileSync(join(small, "SKILL.md"), "---\nname: small\ndescription: A small catalog\n---\n\nSmall body.\n");
		await loader.reload();
		session.setActiveToolsByName(["read"]);
		assert.ok(session.systemPrompt.includes(formatSkillsForPrompt(loader.getSkills().skills, "read")));
		assert.ok(!session.systemPrompt.includes("Skill catalog:"));
	} finally {
		session?.dispose();
		if (previous === undefined) delete process.env.PI_CODING_AGENT_DIR;
		else process.env.PI_CODING_AGENT_DIR = previous;
		rmSync(root, { recursive: true, force: true });
	}
});
