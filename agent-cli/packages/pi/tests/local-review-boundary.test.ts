import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtemp, mkdir, realpath, rm, symlink, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

import { registerLocalReviewBoundary } from "../src/tools/local-review-boundary.ts";

async function createRepo(): Promise<{ root: string; parent: string; dispose: () => Promise<void> }> {
	const parent = await mkdtemp(path.join(os.tmpdir(), "cloudthinker-review-boundary-"));
	const root = path.join(parent, "repo");
	await mkdir(root);
	await writeFile(path.join(root, ".gitignore"), "*.secret\n");
	await writeFile(path.join(root, "src.ts"), "export const value = 1;\n");
	await writeFile(path.join(root, "local.secret"), "private\n");
	execFileSync("git", ["init", "-q", root]);
	return { root, parent, dispose: () => rm(parent, { recursive: true, force: true }) };
}

function captureHandlers(root: string) {
	const handlers = new Map<string, (event: never) => unknown>();
	const pi = {
		on: (event: string, handler: (event: never) => unknown) => handlers.set(event, handler),
	} as unknown as ExtensionAPI;
	registerLocalReviewBoundary(pi, root);
	return handlers;
}

function toolCall(toolName: string, filePath: string) {
	return {
		type: "tool_call",
		toolCallId: "review-call",
		toolName,
		input: { path: filePath },
	};
}

test("local review tools allow repository files and canonicalize their paths", async () => {
	const repo = await createRepo();
	try {
		const handlers = captureHandlers(repo.root);
		const event = toolCall("read", "src.ts");
		const result = await handlers.get("tool_call")!(event as never);

		assert.equal(result, undefined);
		assert.equal((event.input as { path: string }).path, await realpath(path.join(repo.root, "src.ts")));
	} finally {
		await repo.dispose();
	}
});

test("local review canonicalizes a symlinked checkout root", async () => {
	const repo = await createRepo();
	try {
		const alias = path.join(repo.parent, "repo-link");
		await symlink(repo.root, alias);
		const handlers = captureHandlers(alias);
		const event = toolCall("read", "src.ts");
		const result = await handlers.get("tool_call")!(event as never);

		assert.equal(result, undefined);
		assert.equal((event.input as { path: string }).path, await realpath(path.join(repo.root, "src.ts")));
	} finally {
		await repo.dispose();
	}
});

test("local review tools block absolute paths and parent traversal", async () => {
	const repo = await createRepo();
	try {
		const handlers = captureHandlers(repo.root);
		const outside = path.join(repo.parent, "outside.secret");
		await writeFile(outside, "private\n");
		for (const requested of [outside, "../outside.secret"]) {
			const result = await handlers.get("tool_call")!(toolCall("read", requested) as never) as { block: boolean };
			assert.equal(result.block, true);
		}
	} finally {
		await repo.dispose();
	}
});

test("local review tools block symlinks, Git metadata, and ignored files", async () => {
	const repo = await createRepo();
	try {
		const handlers = captureHandlers(repo.root);
		await symlink(path.join(repo.parent, "outside.secret"), path.join(repo.root, "outside-link"));
		await writeFile(path.join(repo.parent, "outside.secret"), "private\n");
		for (const requested of ["outside-link", ".git/config", "local.secret"]) {
			const result = await handlers.get("tool_call")!(toolCall("read", requested) as never) as { block: boolean };
			assert.equal(result.block, true);
		}
	} finally {
		await repo.dispose();
	}
});

test("local review listings and grep results hide Git metadata and ignored paths", async () => {
	const repo = await createRepo();
	try {
		const handlers = captureHandlers(repo.root);
		const result = await handlers.get("tool_result")!({
			type: "tool_result",
			toolCallId: "review-call",
			toolName: "find",
			input: { path: "." },
			content: [{ type: "text", text: ".git/\n.git/config\nsrc.ts\nlocal.secret\napp/.git/HEAD" }],
			isError: false,
		} as never) as { content: Array<{ text: string }> };

		assert.equal(result.content[0]?.text, "src.ts");

		const grepResult = await handlers.get("tool_result")!({
			type: "tool_result",
			toolCallId: "grep-call",
			toolName: "grep",
			input: { path: repo.root, glob: "*.secret" },
			content: [{ type: "text", text: "local.secret:1: private\nsrc.ts:1: public" }],
			isError: false,
		} as never) as { content: Array<{ text: string }> };

		assert.equal(grepResult.content[0]?.text, "src.ts:1: public");
	} finally {
		await repo.dispose();
	}
});
