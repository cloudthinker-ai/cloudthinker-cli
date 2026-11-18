import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import type { ExtensionAPI, ExtensionToolContext, ToolDefinition } from "@earendil-works/pi-coding-agent";
import type { Component } from "@earendil-works/pi-tui";

import {
	CT_CONFIG,
	SETTING_DECLINED,
	SETTING_KEYS,
	backupPath,
	headlessRefusal,
	registerConfig,
	settingsPath,
} from "../src/tools/ct-config.ts";
import { resultBody } from "../src/tools/render.ts";

const theme = { fg: (_color: string, value: string) => value, bold: (value: string) => value };

interface Harness {
	run(params: Record<string, string>, ui?: Partial<UiOptions>): Promise<string>;
	prompts: string[][];
	root: string;
	settings(): Record<string, unknown> | undefined;
}

interface UiOptions {
	hasUI: boolean;
	key: string;
	trusted: boolean;
}

function withAgent(work: (h: Harness) => Promise<void>): () => Promise<void> {
	return async () => {
		const root = mkdtempSync(join(tmpdir(), "ct-config-"));
		const previous = process.env.PI_CODING_AGENT_DIR;
		process.env.PI_CODING_AGENT_DIR = join(root, "agent");
		mkdirSync(join(root, "project"), { recursive: true });
		let tool: ToolDefinition | undefined;
		registerConfig({ registerTool: (definition: ToolDefinition) => { tool = definition; } } as unknown as ExtensionAPI);
		const prompts: string[][] = [];
		const run = async (params: Record<string, string>, options: Partial<UiOptions> = {}): Promise<string> => {
			const ui = { hasUI: true, key: "y", trusted: true, ...options };
			const ctx = {
				cwd: join(root, "project"),
				hasUI: ui.hasUI,
				mode: ui.hasUI ? "tui" : "print",
				isProjectTrusted: () => ui.trusted,
				ui: {
					custom: async <T>(factory: (tui: unknown, theme: unknown, keybindings: unknown, done: (value: T) => void) => Component): Promise<T> => {
						let chosen: T | undefined;
						const component = factory({ requestRender: () => {}, terminal: { rows: 40 } }, theme, {}, (value) => { chosen = value; }) as Component & { handleInput(data: string): void };
						prompts.push(component.render(100));
						component.handleInput(ui.key);
						return chosen as T;
					},
				},
			} as unknown as ExtensionToolContext;
			const result = await tool!.execute("call-1", params as never, undefined, undefined, ctx);
			return resultBody(result);
		};
		const settings = () => existsSync(settingsPath()) ? JSON.parse(readFileSync(settingsPath(), "utf8")) as Record<string, unknown> : undefined;
		try {
			assert.equal(tool?.name, CT_CONFIG);
			await work({ run, prompts, root, settings });
		} finally {
			if (previous === undefined) delete process.env.PI_CODING_AGENT_DIR;
			else process.env.PI_CODING_AGENT_DIR = previous;
			rmSync(root, { recursive: true, force: true });
		}
	};
}

function seed(value: unknown): void {
	mkdirSync(join(process.env.PI_CODING_AGENT_DIR!), { recursive: true });
	writeFileSync(settingsPath(), typeof value === "string" ? value : JSON.stringify(value));
}

test("CT-CONFIG-1: list names every key with its type, allowed values and meaning", withAgent(async (h) => {
	seed({ toolOutput: "preview" });
	const body = await h.run({ action: "list" });
	assert.match(body, /toolOutput: preview · allowed: compact, preview · Show tool results/);
	assert.match(body, /cloudDefault: true \(default\) · allowed: true, false · Start new sessions with Cloud on\./);
	assert.equal(body.split("\n").length, SETTING_KEYS.length + 1);
}));

test("CT-CONFIG-2: login, token, key, server and shell settings are not exposed", withAgent(async (h) => {
	for (const entry of SETTING_KEYS) assert.doesNotMatch(entry.key, /token|auth|login|url|api.?key|password|secret|proxy|shell|model|theme|tuiMode/i);
	for (const key of ["default_url", "httpProxy", "shellCommandPrefix", "defaultModel", "defaultProvider", "theme", "tuiMode"]) {
		await assert.rejects(h.run({ action: "get", key }), /Unknown setting/);
		await assert.rejects(h.run({ action: "set", key, value: "x" }), /Unknown setting/);
	}
	assert.equal(h.prompts.length, 0);
	assert.equal(h.settings(), undefined);
}));

test("CT-CONFIG-3: a value outside the key's allowed values is refused before any write", withAgent(async (h) => {
	await assert.rejects(h.run({ action: "set", key: "toolOutput", value: "verbose" }), /"verbose" is not a valid value for toolOutput\. Allowed: compact, preview\./);
	await assert.rejects(h.run({ action: "set", key: "cloudDefault", value: "yes" }), /Allowed: true, false/);
	assert.equal(h.prompts.length, 0);
	assert.equal(h.settings(), undefined);
	assert.equal(existsSync(backupPath()), false);
}));

test("CT-CONFIG-4: set shows old to new in the approval card and saves only after Save", withAgent(async (h) => {
	seed({ theme: "dark", markdown: { mermaidBorderColors: true } });
	const body = await h.run({ action: "set", key: "markdown.mermaid", value: "final" });
	assert.equal(h.prompts.length, 1);
	const card = h.prompts[0]!.join("\n");
	assert.match(card, /approval needed: change a setting/);
	assert.match(card, /markdown\.mermaid: not set → final/);
	assert.match(card, /1\. Save/);
	assert.match(body, /^Saved markdown\.mermaid: not set → final/);
	assert.deepEqual(h.settings(), { theme: "dark", markdown: { mermaidBorderColors: true, mermaid: "final" } });
	assert.deepEqual(JSON.parse(readFileSync(backupPath(), "utf8")), { key: "markdown.mermaid", before: null, after: "final" });
	assert.equal(await h.run({ action: "set", key: "markdown.mermaid", value: "final" }), "markdown.mermaid is already final; nothing changed.");
	assert.equal(h.prompts.length, 1);
}));

test("CT-CONFIG-5: Esc declines, and nothing is written", withAgent(async (h) => {
	seed({ cloudDefault: true });
	await assert.rejects(h.run({ action: "set", key: "cloudDefault", value: "false" }, { key: "\x1b" }), (error: Error) => error.message === SETTING_DECLINED);
	assert.deepEqual(h.settings(), { cloudDefault: true });
	assert.equal(existsSync(backupPath()), false);
}));

test("CT-CONFIG-6: undo asks again and restores the value before the last set", withAgent(async (h) => {
	seed({ toolOutput: "compact", theme: "dark" });
	await h.run({ action: "set", key: "toolOutput", value: "preview" });
	const body = await h.run({ action: "undo" });
	assert.equal(h.prompts.length, 2);
	assert.match(h.prompts[1]!.join("\n"), /toolOutput: preview → compact/);
	assert.match(body, /^Restored toolOutput: preview → compact\./);
	assert.deepEqual(h.settings(), { toolOutput: "compact", theme: "dark" });
	assert.equal(existsSync(backupPath()), false);
	await assert.rejects(h.run({ action: "undo" }), /There is no ct_config change to undo/);
}));

test("CT-CONFIG-7: undo of a key added by set removes it again", withAgent(async (h) => {
	await h.run({ action: "set", key: "diffStyle", value: "split" });
	assert.deepEqual(h.settings(), { diffStyle: "split" });
	await h.run({ action: "undo" });
	assert.match(h.prompts[1]!.join("\n"), /diffStyle: split → not set/);
	assert.deepEqual(h.settings(), {});
}));

test("CT-CONFIG-8: undo refuses when the user changed the key since", withAgent(async (h) => {
	await h.run({ action: "set", key: "diffStyle", value: "split" });
	seed({ diffStyle: "unified" });
	await assert.rejects(h.run({ action: "undo" }), /diffStyle is now unified, not the split that ct_config saved/);
	assert.equal(h.prompts.length, 1);
	assert.deepEqual(h.settings(), { diffStyle: "unified" });
}));

test("CT-CONFIG-9: headless set and undo are refused without writing; reads still work", withAgent(async (h) => {
	seed({ toolOutput: "compact" });
	await assert.rejects(h.run({ action: "set", key: "toolOutput", value: "preview" }, { hasUI: false }), (error: Error) => error.message === headlessRefusal("set"));
	assert.deepEqual(h.settings(), { toolOutput: "compact" });
	assert.equal(existsSync(backupPath()), false);
	assert.match(await h.run({ action: "get", key: "toolOutput" }, { hasUI: false }), /^toolOutput: compact/);
	assert.equal(h.prompts.length, 0);
	await h.run({ action: "set", key: "toolOutput", value: "preview" });
	await assert.rejects(h.run({ action: "undo" }, { hasUI: false }), (error: Error) => error.message === headlessRefusal("undo"));
	assert.deepEqual(h.settings(), { toolOutput: "preview" });
	assert.equal(existsSync(backupPath()), true);
}));

test("CT-CONFIG-10: an unreadable settings file is never overwritten", withAgent(async (h) => {
	seed("{ not json");
	await assert.rejects(h.run({ action: "set", key: "toolOutput", value: "preview" }), /is not a valid JSON object, so no setting was changed/);
	assert.equal(readFileSync(settingsPath(), "utf8"), "{ not json");
	assert.equal(h.prompts.length, 0);
}));

test("CT-CONFIG-11: get says when a trusted project's settings override the saved value", withAgent(async (h) => {
	seed({ cloudDefault: true });
	mkdirSync(join(h.root, "project", ".pi"), { recursive: true });
	writeFileSync(join(h.root, "project", ".pi", "settings.json"), JSON.stringify({ cloudDefault: false }));
	assert.match(await h.run({ action: "get", key: "cloudDefault" }), /This project's \.pi\/settings\.json sets cloudDefault to false, which wins in this folder\./);
	assert.doesNotMatch(await h.run({ action: "get", key: "cloudDefault" }, { trusted: false }), /wins in this folder/);
}));
