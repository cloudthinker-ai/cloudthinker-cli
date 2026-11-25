import { existsSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { getPackageDir } from "@earendil-works/pi-coding-agent";

export const TOUR_OFFER = "New here? /skill:tour helps you get started with Local and Cloud";

function bundledSkillPath(name: string, packageDir: string, moduleUrl: string): string {
	const bundled = join(packageDir, ".agents", "skills", name, "SKILL.md");
	if (existsSync(bundled)) return bundled;
	return fileURLToPath(new URL(`../../agent/.agents/skills/${name}/SKILL.md`, moduleUrl));
}

export function bundledTourPath(packageDir: string = getPackageDir()): string {
	return bundledSkillPath("tour", packageDir, import.meta.url);
}

export function bundledDoctorPath(packageDir: string = getPackageDir()): string {
	return bundledSkillPath("doctor", packageDir, import.meta.url);
}

export function bundledReviewSetupPath(
	packageDir: string = getPackageDir(),
	moduleUrl: string = import.meta.url,
): string {
	const local = bundledSkillPath("review-setup", packageDir, moduleUrl);
	if (existsSync(local)) return local;
	return fileURLToPath(new URL("../../../../executor/app/skills/public/review-setup/SKILL.md", moduleUrl));
}
