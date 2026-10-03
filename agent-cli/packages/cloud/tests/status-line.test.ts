import assert from "node:assert/strict";
import test from "node:test";
import { stripVTControlCharacters } from "node:util";

import { initTheme } from "@earendil-works/pi-coding-agent";
import type { ExtensionAPI, ReadonlyFooterDataProvider, Theme } from "@earendil-works/pi-coding-agent";

import type { CloudThinkerClient } from "../src/client.ts";
import { CLOUD_ENTRY_TYPE, CREDITS_KEY, CloudThinkerRuntime, STATUS_KEY } from "../src/runtime.ts";
import { statusLineFooter } from "../src/status-line.ts";
import { hostVersionsFrom } from "../src/versions.ts";

initTheme("dark");
const plainTheme = { fg: (_color: string, text: string) => text, bold: (text: string) => text } as unknown as Theme;

function footer(runtime: CloudThinkerRuntime, statuses: [string, string][]) {
	const data = {
		getGitBranch: () => "main",
		getExtensionStatuses: () => new Map(statuses),
		getAvailableProviderCount: () => 1,
		onBranchChange: () => () => {},
	} as unknown as ReadonlyFooterDataProvider;
	const ctx = { cwd: "/srv/repo", model: { id: "pro", name: "Pro" }, getContextUsage: () => ({ percent: 41.6 }) } as never;
	return (width: number) => stripVTControlCharacters(statusLineFooter(runtime, ctx)(undefined, plainTheme, data).render(width).join("\n"));
}

test("the pinned status line names the workspace, approval mode, credits, and Connections, and keeps other statuses", () => {
	const runtime = new CloudThinkerRuntime(
		{ appendEntry: () => {}, getActiveTools: () => [], setActiveTools: () => {} } as unknown as ExtensionAPI,
		{} as CloudThinkerClient,
		hostVersionsFrom({ version: "0.4.0", piVersion: "0.85.1" }, "0.4.0"),
	);
	const render = footer(runtime, [
		[CLOUD_ENTRY_TYPE, "[L+C]"],
		[CREDITS_KEY, "◆ 3 credits"],
		[STATUS_KEY, "mirror offline (2 pending)"],
		["ct-background", "1 running command"],
	]);
	assert.match(render(100), /○ connecting…/);
	runtime.linkFailure = { kind: "signed-out", label: "signed out", cause: "", action: "", detail: "", retry: false };
	assert.match(render(100), /signed out/);
	runtime.linkFailure = undefined;

	runtime.session = { conversation_id: "c-1", workspace_id: "w-1", web_url: "https://example.com/c-1", auto_mode: { enabled: true, can_edit: true } };
	runtime.identity = { user_email: "primary@example.com", workspace_id: "w-1", workspace_name: "primary", organization_id: null };
	runtime.setAutoMode({ enabled: true, canEdit: true });
	runtime.credits = { credits_used: 12.5, tokens_consumed: 1 };
	runtime.connections = { xml: "", prefixes: ["aws", "k8s", "github", "gcp"] };
	const [first, second] = render(120).split("\n");
	assert.match(first!, /● primary · Auto · ◆ 12\.5 credits · aws, k8s, github \+1 +~?\/?srv\/repo \(main\) $/);
	assert.doesNotMatch(first!, /\[L\+C\]/);
	assert.match(second!, /mirror offline \(2 pending\) · 1 running command +Pro · 42% context $/);
	assert.doesNotMatch(second!, /◆ 3 credits/);

	runtime.setAutoMode({ enabled: false, canEdit: true });
	assert.match(render(120), /primary · Manual ·/);
	runtime.cloudEnabled = false;
	assert.match(render(120), /○ Cloud off/);
});
