import { createHash, randomUUID } from "node:crypto";
import { existsSync, mkdirSync, renameSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import { AgentSession, formatSkillsForPrompt, getAgentDir, type Skill } from "@earendil-works/pi-coding-agent";

export const SKILL_CATALOG_BYTES = 8192;

type PromptOptions = { skills: Skill[]; sections: Record<string, string> };

type PromptHost = {
	_resourceLoader: AgentSession["resourceLoader"];
	_baseSystemPromptOptions: PromptOptions;
	_rebuildSystemPrompt: (tools: string[]) => void;
};

export function budgetSkillCatalog(skills: Skill[], readTool: "read" | "bash", agentDir = getAgentDir()): { skills: Skill[]; guidance: string } {
	const visible = skills.filter((skill) => !skill.disableModelInvocation);
	if (Buffer.byteLength(formatSkillsForPrompt(visible, readTool), "utf8") <= SKILL_CATALOG_BYTES) {
		return { skills: visible, guidance: "" };
	}
	const index = visible.map((skill) => JSON.stringify({ name: skill.name, description: skill.description, location: skill.filePath })).join("\n") + "\n";
	const digest = createHash("sha256").update(index).digest("hex");
	const directory = join(agentDir, "cloudthinker", "skill-catalogs");
	mkdirSync(directory, { recursive: true, mode: 0o700 });
	const location = join(directory, `${digest}.jsonl`);
	if (!existsSync(location)) {
		const temporary = join(directory, `${digest}.${randomUUID()}.tmp`);
		writeFileSync(temporary, index, { mode: 0o600 });
		renameSync(temporary, location);
	}
	const guidance = `\n\nSkill catalog: some descriptions are omitted to preserve context. The complete automatically available catalog is at ${JSON.stringify(location)} (JSONL: name, description, location). Before deciding a skill is unavailable, search this file using an existing local search tool or bash (rg -i -m 8, falling back to grep), or read it in short pages. Read the matching location's full SKILL.md before using the skill. Explicit /skill:name invocation remains available.\n`;
	const selected: Skill[] = [];
	for (const skill of visible) {
		const candidate = [...selected, skill];
		if (Buffer.byteLength(formatSkillsForPrompt(candidate, readTool) + guidance, "utf8") <= SKILL_CATALOG_BYTES) selected.push(skill);
	}
	if (Buffer.byteLength(guidance, "utf8") > SKILL_CATALOG_BYTES) throw new Error("Skill catalog discovery path exceeds the context budget");
	return { skills: selected, guidance };
}

let installed = false;

export function applySkillCatalogGuard(): void {
	if (installed) return;
	const prototype = AgentSession.prototype as unknown as PromptHost;
	const original = prototype._rebuildSystemPrompt;
	if (typeof original !== "function") throw new Error("Pi no longer defines _rebuildSystemPrompt; skill catalog budgeting cannot be installed");
	prototype._rebuildSystemPrompt = function (tools) {
		original.call(this, tools);
		const readTool = tools.includes("read") ? "read" : tools.includes("bash") ? "bash" : undefined;
		if (!readTool) return;
		const catalog = budgetSkillCatalog(this._resourceLoader.getSkills().skills, readTool);
		if (!catalog.guidance) return;
		const options = this._baseSystemPromptOptions;
		this._baseSystemPromptOptions = { ...options, skills: catalog.skills, sections: { ...options.sections, skill_catalog: catalog.guidance.trim() } };
	};
	installed = true;
}
