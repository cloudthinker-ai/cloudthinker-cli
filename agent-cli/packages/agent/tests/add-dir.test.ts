import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

import { additionalFoldersBlock, extractAddDirs, folderInstructions, registerAddDirs } from "../src/add-dir.ts";

function workspace() {
	const root = realpathSync(mkdtempSync(join(tmpdir(), "ct-add-dir-")));
	const main = join(root, "main");
	const other = join(root, "other");
	const third = join(root, "third");
	for (const dir of [main, other, third]) mkdirSync(dir);
	writeFileSync(join(other, "AGENTS.md"), "Use tabs in other.");
	writeFileSync(join(third, "CLAUDE.md"), "Third rules.");
	writeFileSync(join(root, "notes.txt"), "a file");
	return { root, main, other, third, done: () => rmSync(root, { recursive: true, force: true }) };
}

test("--add-dir paths resolve against the working folder, repeat, dedupe, and leave other args alone", () => {
	const w = workspace();
	try {
		const result = extractAddDirs(["-p", "hi", "--add-dir", "../other", "--add-dir", w.third, "--add-dir", "../other", "--add-dir", "."], w.main);
		assert.deepEqual(result, { dirs: [w.other, w.third], rest: ["-p", "hi"] });
	} finally {
		w.done();
	}
});

test("--add-dir names the bad path when it is missing or a file", () => {
	const w = workspace();
	try {
		assert.match(extractAddDirs(["--add-dir", "../nope"], w.main).error ?? "", /--add-dir \.\.\/nope: not a folder/);
		assert.match(extractAddDirs(["--add-dir", "../notes.txt"], w.main).error ?? "", /not a folder/);
	} finally {
		w.done();
	}
});

test("instructions from an added folder load like the main folder's and are not loaded twice", () => {
	const w = workspace();
	try {
		const agentDir = join(w.root, "agent");
		mkdirSync(agentDir);
		const found = folderInstructions([w.other, w.third], [], agentDir);
		assert.deepEqual(found.map((file) => file.path), [join(w.other, "AGENTS.md"), join(w.third, "CLAUDE.md")]);
		assert.deepEqual(folderInstructions([w.other], found, agentDir), []);
	} finally {
		w.done();
	}
});

test("the system context names each folder and carries its instructions, fixed for the whole run", () => {
	const w = workspace();
	try {
		const agentDir = join(w.root, "agent");
		mkdirSync(agentDir);
		const handlers: ((event: unknown, ctx: unknown) => { systemPrompt: string })[] = [];
		const pi = { on: (_name: string, handler: (typeof handlers)[number]) => { handlers.push(handler); } } as unknown as Pick<ExtensionAPI, "on">;
		registerAddDirs(pi, [w.other], agentDir);
		const event = { systemPrompt: "BASE", systemPromptOptions: { contextFiles: [] } };
		const trusted = { isProjectTrusted: () => true };
		const first = handlers[0]?.(event, trusted).systemPrompt ?? "";
		assert.ok(first.startsWith("BASE\n\n<additional_folders>"));
		assert.ok(first.includes(`- ${w.other}`));
		assert.ok(first.includes(`<project_instructions path="${join(w.other, "AGENTS.md")}">\nUse tabs in other.`));
		writeFileSync(join(w.other, "AGENTS.md"), "changed");
		assert.equal(handlers[0]?.(event, trusted).systemPrompt, first);
		const untrusted = { systemPrompt: "BASE", systemPromptOptions: { contextFiles: [] } };
		const bare: string[] = [];
		const pi2 = { on: (_name: string, handler: (typeof handlers)[number]) => { bare.push(handler(untrusted, { isProjectTrusted: () => false }).systemPrompt); } } as unknown as Pick<ExtensionAPI, "on">;
		registerAddDirs(pi2, [w.other], agentDir);
		assert.equal(bare[0], `BASE\n\n${additionalFoldersBlock([w.other], [])}`);
	} finally {
		w.done();
	}
});

test("no --add-dir registers nothing", () => {
	const registered: string[] = [];
	registerAddDirs({ on: (name: string) => { registered.push(name); } } as unknown as Pick<ExtensionAPI, "on">, [], "/unused");
	assert.deepEqual(registered, []);
});
