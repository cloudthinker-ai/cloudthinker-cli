import { statSync, realpathSync } from "node:fs";
import { resolve } from "node:path";

import { loadProjectContextFiles, type ExtensionAPI } from "@earendil-works/pi-coding-agent";

import { takeFlagValues } from "./argv.ts";

export const ADD_DIR_FLAG = "--add-dir";

export interface ContextFile {
	path: string;
	content: string;
}

export function addDirError(path: string): string {
	return `${ADD_DIR_FLAG} ${path}: not a folder. Pass an existing folder, for example ${ADD_DIR_FLAG} ../other-repo.`;
}

export function extractAddDirs(argv: readonly string[], cwd: string): { dirs: string[]; rest: string[]; error?: string } {
	const { values, rest } = takeFlagValues(argv, ADD_DIR_FLAG);
	const main = realpathSync(cwd);
	const dirs: string[] = [];
	for (const value of values) {
		let dir: string;
		try {
			dir = realpathSync(resolve(cwd, value));
			if (!statSync(dir).isDirectory()) return { dirs, rest, error: addDirError(value) };
		} catch {
			return { dirs, rest, error: addDirError(value) };
		}
		if (dir !== main && !dirs.includes(dir)) dirs.push(dir);
	}
	return { dirs, rest };
}

export function additionalFoldersBlock(dirs: readonly string[], files: readonly ContextFile[]): string {
	const lines = [
		"<additional_folders>",
		"The user added these folders to this session. Besides the working folder, you may read and edit files in them; name a file there by absolute path.",
		...dirs.map((dir) => `- ${dir}`),
		"</additional_folders>",
		...files.map(({ path, content }) => `<project_instructions path="${path}">\n${content}\n</project_instructions>`),
	];
	return lines.join("\n");
}

export function folderInstructions(dirs: readonly string[], known: readonly ContextFile[], agentDir: string): ContextFile[] {
	const seen = new Set(known.map((file) => file.path));
	const found: ContextFile[] = [];
	for (const dir of dirs) {
		for (const file of loadProjectContextFiles({ cwd: dir, agentDir })) {
			if (seen.has(file.path)) continue;
			seen.add(file.path);
			found.push(file);
		}
	}
	return found;
}

export function registerAddDirs(pi: Pick<ExtensionAPI, "on">, dirs: readonly string[], agentDir: string): void {
	if (dirs.length === 0) return;
	let block: string | undefined;
	pi.on("before_agent_start", (event, ctx) => {
		block ??= additionalFoldersBlock(
			dirs,
			ctx.isProjectTrusted() ? folderInstructions(dirs, event.systemPromptOptions.contextFiles ?? [], agentDir) : [],
		);
		return { systemPrompt: `${event.systemPrompt}\n\n${block}` };
	});
}
