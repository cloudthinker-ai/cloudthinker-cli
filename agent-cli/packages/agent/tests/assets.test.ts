import assert from "node:assert/strict";
import { cpSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

import { assetFiles, validateAssets } from "../scripts/validate-assets.ts";

const piRoot = dirname(dirname(fileURLToPath(import.meta.resolve("@earendil-works/pi-coding-agent"))));
const subagentsLicense = readFileSync(new URL("../node_modules/@cloudthinker/subagents/LICENSE", import.meta.url), "utf8");
const notice = subagentsLicense + readFileSync(new URL("../node_modules/@narumitw/pi-btw/LICENSE", import.meta.url), "utf8");

test("CA-AD-6 real pi assets validate, and a missing theme or unexpected entry fails", () => {
	const root = mkdtempSync(join(tmpdir(), "ct-assets-"));
	try {
		for (const [source, target] of [
			["dist/modes/interactive/theme", "theme"], ["dist/modes/interactive/assets", "assets"],
			["docs", "docs"], ["examples", "examples"],
		]) {
			cpSync(join(piRoot, source!), join(root, target!), {
				recursive: true,
				filter: (path) => lstatSync(path).isDirectory() || (target === "theme" ? path.endsWith(".json") : target === "assets" ? path.endsWith(".png") : true),
			});
		}
		cpSync(new URL("../.agents/skills/", import.meta.url), join(root, ".agents/skills"), { recursive: true });
		mkdirSync(join(root, "export-html"));
		for (const file of ["template.html", "template.css", "template.js", "vendor"]) {
			cpSync(join(piRoot, "dist/core/export-html", file), join(root, "export-html", file), { recursive: true });
		}
		cpSync(new URL("../themes/", import.meta.url), join(root, "theme"), { recursive: true });
		for (const file of ["cloudthinker-agent", "NOTICE", "CHANGELOG.md", "photon_rs_bg.wasm"]) writeFileSync(join(root, file), "fixture");
		writeFileSync(join(root, "NOTICE"), notice);
		writeFileSync(join(root, "package.json"), JSON.stringify({ piVersion: "1.0.0", piConfig: { name: "cloudthinker" } }));
		validateAssets(root, piRoot);
		const tourPath = join(root, ".agents/skills/tour/SKILL.md");
		rmSync(tourPath);
		assert.throws(() => validateAssets(root, piRoot), /Missing bundle asset: .agents\/skills\/tour\/SKILL.md/);
		writeFileSync(tourPath, "");
		assert.throws(() => validateAssets(root, piRoot), /Empty bundle asset: .agents\/skills\/tour\/SKILL.md/);
		cpSync(new URL("../.agents/skills/tour/SKILL.md", import.meta.url), tourPath);
		writeFileSync(join(root, "NOTICE"), "pi only");
		assert.throws(() => validateAssets(root, piRoot), /Missing pi-subagents license notice/);
		writeFileSync(join(root, "NOTICE"), subagentsLicense);
		assert.throws(() => validateAssets(root, piRoot), /Missing pi-btw license notice/);
		writeFileSync(join(root, "NOTICE"), notice);
		rmSync(join(root, "theme/dark.json"));
		assert.throws(() => validateAssets(root, piRoot), /Missing bundle asset/);
		cpSync(join(piRoot, "dist/modes/interactive/theme/dark.json"), join(root, "theme/dark.json"));
		writeFileSync(join(root, "unexpected.txt"), "extra");
		assert.throws(() => validateAssets(root, piRoot), /Unexpected bundle asset/);
		rmSync(join(root, "unexpected.txt"));
		mkdirSync(join(root, "unexpected-empty"));
		assert.throws(() => validateAssets(root, piRoot), /Unexpected bundle directory/);
	} finally {
		rmSync(root, { recursive: true, force: true });
	}
});

test("CA-AD-6 symlinks and cache directories cannot enter the asset inventory", () => {
	const root = mkdtempSync(join(tmpdir(), "ct-assets-"));
	try {
		symlinkSync(piRoot, join(root, "linked"), "dir");
		assert.throws(() => assetFiles(root), /symlink/);
		rmSync(join(root, "linked"));
		mkdirSync(join(root, "node_modules"));
		assert.throws(() => assetFiles(root), /Unexpected bundle entry/);
	} finally {
		rmSync(root, { recursive: true, force: true });
	}
});
