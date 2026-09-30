import { existsSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { getPackageDir } from "@earendil-works/pi-coding-agent";

export const TOUR_OFFER = "New here? /skill:tour helps you get started with Local and Cloud";

export function bundledTourPath(packageDir: string = getPackageDir()): string {
	const bundled = join(packageDir, ".agents", "skills", "tour", "SKILL.md");
	if (existsSync(bundled)) return bundled;
	return fileURLToPath(new URL("../../agent/.agents/skills/tour/SKILL.md", import.meta.url));
}
