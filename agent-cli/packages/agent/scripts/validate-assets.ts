import { lstatSync, readdirSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const REQUIRED = [
	"cloudthinker-agent", "package.json", "NOTICE", "photon_rs_bg.wasm",
	"theme/dark.json", "theme/light.json", "theme/theme-schema.json",
	"theme/cloudthinker-dark.json", "theme/cloudthinker-light.json",
	"assets/clankolas.png", "export-html/template.html", "export-html/template.css",
	"export-html/template.js", "export-html/vendor/marked.min.js",
	"export-html/vendor/highlight.min.js", "docs/usage.md", "docs/extensions.md",
	"examples/rpc-extension-ui.ts",
];

export function assetFiles(root: string, prefix = ""): string[] {
	const files: string[] = [];
	for (const name of readdirSync(join(root, prefix))) {
		const relative = prefix ? `${prefix}/${name}` : name;
		if (/^(node_modules|\.venv|__pycache__|\.git)$|\.pyc$|\.egg-info$|^\.env(?:\.|$)/.test(name)) {
			throw new Error(`Unexpected bundle entry: ${relative}`);
		}
		const stat = lstatSync(join(root, relative));
		if (stat.isSymbolicLink()) throw new Error(`Bundle symlink: ${relative}`);
		if (stat.isDirectory()) files.push(...assetFiles(root, relative));
		else if (stat.isFile()) files.push(relative);
		else throw new Error(`Unsupported bundle entry: ${relative}`);
	}
	return files.sort();
}

export function validateAssets(bundle: string, piRoot: string): void {
	const expected = new Set(REQUIRED);
	for (const directory of ["docs", "examples"]) {
		for (const file of assetFiles(join(piRoot, directory))) expected.add(`${directory}/${file}`);
	}
	const files = new Set(assetFiles(bundle));
	for (const file of expected) {
		if (!files.has(file)) throw new Error(`Missing bundle asset: ${file}`);
		if (lstatSync(join(bundle, file)).size === 0) throw new Error(`Empty bundle asset: ${file}`);
	}
	for (const file of files) {
		if (!expected.has(file)) throw new Error(`Unexpected bundle asset: ${file}`);
	}
	const checkDirectories = (prefix: string): void => {
		for (const name of readdirSync(join(bundle, prefix))) {
			const relative = prefix ? `${prefix}/${name}` : name;
			if (!lstatSync(join(bundle, relative)).isDirectory()) continue;
			if (![...expected].some((file) => file.startsWith(`${relative}/`))) {
				throw new Error(`Unexpected bundle directory: ${relative}`);
			}
			checkDirectories(relative);
		}
	};
	checkDirectories("");
	const manifest = JSON.parse(readFileSync(join(bundle, "package.json"), "utf8"));
	if (manifest.piVersion !== "0.85.1" || manifest.piConfig?.name !== "cloudthinker") {
		throw new Error("Incompatible bundle manifest");
	}
	if (!readFileSync(join(bundle, "NOTICE"), "utf8").includes("Copyright (c) 2026 tintinweb")) {
		throw new Error("Missing pi-subagents license notice");
	}
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
	const [bundle, piRoot] = process.argv.slice(2);
	if (!bundle || !piRoot) throw new Error("Usage: validate-assets.ts <bundle> <pi-root>");
	validateAssets(bundle, piRoot);
}
