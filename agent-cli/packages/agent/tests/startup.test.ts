import assert from "node:assert/strict";
import test from "node:test";
import { stripVTControlCharacters } from "node:util";

import { InteractiveMode, initTheme } from "@earendil-works/pi-coding-agent";
import { Container } from "@earendil-works/pi-tui";

import { applyStartupUi } from "../src/startup.ts";

initTheme("dark");
applyStartupUi();

function startup({ quiet = false, verbose = false, issues = true } = {}) {
	const host = Object.create(InteractiveMode.prototype);
	Object.assign(host, {
		loadedResourcesContainer: new Container(),
		options: { verbose },
		toolOutputExpanded: false,
		getBuiltInCommandConflictDiagnostics: () => [],
		runtimeHost: { session: {
			settingsManager: { getQuietStartup: () => quiet },
			sessionManager: { getCwd: () => "/workspace" },
			promptTemplates: [],
			resourceLoader: {
				getSystemPromptSource: () => undefined,
				getAppendSystemPromptSources: () => [],
				getAgentsFiles: () => ({ agentsFiles: [{ path: "/workspace/AGENTS.md" }] }),
				getSkills: () => ({
					skills: Array.from({ length: 150 }, (_,index) => ({ name: `skill-${index}`, filePath: `/workspace/skills/skill-${index}/SKILL.md` })),
					diagnostics: issues ? [{ type: "warning", message: "Duplicate skill retained project copy", path: "/workspace/skills/skill-0/SKILL.md" }] : [],
				}),
				getPrompts: () => ({ prompts: [], diagnostics: [] }),
				getThemes: () => ({ themes: [], diagnostics: [] }),
				getExtensions: () => ({ extensions: [], errors: issues ? [{ path: "/workspace/broken.ts", error: "Extension failed to load" }] : [] }),
			},
			extensionRunner: { getCommandDiagnostics: () => [], getShortcutDiagnostics: () => [] },
		} },
	});
	host.showLoadedResources({ showDiagnosticsWhenQuiet: true });
	return host as { loadedResourcesContainer: Container; showLoadedResources(options: object): void };
}

function rendered(host: ReturnType<typeof startup>): string {
	return stripVTControlCharacters(host.loadedResourcesContainer.render(90).join("\n"));
}

test("large startup inventories collapse, retain error severity, and expand all original details", () => {
	const host = startup();
	const compact = rendered(host);
	assert.equal(compact.split("\n").length, 2);
	assert.match(compact, /150 skills/);
	assert.match(compact, /1 startup error/);
	assert.match(compact, /1 startup warning/);
	const component = host.loadedResourcesContainer.children[0] as unknown as { setExpanded(expanded: boolean): void };
	component.setExpanded(true);
	const detailed = rendered(host);
	assert.match(detailed, /skill-149/);
	assert.match(detailed, /Duplicate skill retained project copy/);
	assert.match(detailed, /Extension failed to load/);
	component.setExpanded(false);
	assert.equal(rendered(host), compact);
	host.showLoadedResources({ showDiagnosticsWhenQuiet: true });
	assert.equal(rendered(host), compact);
});

test("quiet startup shows only diagnostics and verbose startup exposes the full inventory", () => {
	assert.equal(rendered(startup({ quiet: true, issues: false })), "");
	const quiet = rendered(startup({ quiet: true }));
	assert.match(quiet, /1 startup error/);
	assert.doesNotMatch(quiet, /150 skills/);
	assert.match(rendered(startup({ quiet: true, verbose: true })), /skill-149/);
});
