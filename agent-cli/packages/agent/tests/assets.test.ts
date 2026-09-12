import assert from "node:assert/strict";
import { cpSync, lstatSync, mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

import { assetFiles, validateAssets } from "../scripts/validate-assets.ts";

const piRoot = dirname(dirname(fileURLToPath(import.meta.resolve("@earendil-works/pi-coding-agent"))));

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
		mkdirSync(join(root, "export-html"));
		for (const file of ["template.html", "template.css", "template.js", "vendor"]) {
			cpSync(join(piRoot, "dist/core/export-html", file), join(root, "export-html", file), { recursive: true });
		}
		cpSync(new URL("../themes/", import.meta.url), join(root, "theme"), { recursive: true });
		for (const file of ["cloudthinker-agent", "NOTICE", "photon_rs_bg.wasm"]) writeFileSync(join(root, file), "fixture");
		cpSync(new URL("../node_modules/@tintinweb/pi-subagents/LICENSE", import.meta.url), join(root, "NOTICE"));
		writeFileSync(join(root, "package.json"), JSON.stringify({ piVersion: "0.85.1", piConfig: { name: "cloudthinker" } }));
		validateAssets(root, piRoot);
		writeFileSync(join(root, "NOTICE"), "pi only");
		assert.throws(() => validateAssets(root, piRoot), /Missing pi-subagents license notice/);
		cpSync(new URL("../node_modules/@tintinweb/pi-subagents/LICENSE", import.meta.url), join(root, "NOTICE"));
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
