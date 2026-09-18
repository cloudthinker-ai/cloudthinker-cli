import assert from "node:assert/strict";
import test from "node:test";

import { stripVTControlCharacters } from "node:util";
import type { Theme } from "@earendil-works/pi-coding-agent";
import type { Component } from "@earendil-works/pi-tui";

import {
	CLOUD_TAG,
	IDENTITY_UNKNOWN,
	LOCAL_TAG,
	TAG_LEGEND,
	localMissingFile,
	machineBarLines,
	createLegend,
	setMachineState,
	taggedComponent,
} from "../src/awareness.ts";

const theme = { fg: (_color: string, value: string) => value } as unknown as Theme;
const plain = { local: (text: string) => text, cloud: (text: string) => text };

function textComponent(lines: string[]): Component {
	return { render: () => lines, invalidate: () => {} };
}

test("CA-AWARE-1: the machine bar names both machines, the workspace, and the Cloud state", () => {
	setMachineState({ cwd: "/repo", linked: true, workspaceName: "acme-prod", connectionCount: 2, cloudEnabled: true });
	assert.deepEqual(machineBarLines(plain), [
		"[L] this machine  /repo   bash - read - write - edit",
		"[C] sandbox       workspace acme-prod - 2 connections   ct_sandbox_read/write",
		"Cloud: On - /cloud on|off - /open to watch in the browser",
	]);
});

test("CA-AWARE-2: Cloud off states the local-only state and does not advertise sandbox details", () => {
	setMachineState({ cwd: "/repo", linked: true, workspaceName: "acme-prod", connectionCount: 2, cloudEnabled: false });
	const lines = machineBarLines(plain);
	assert.match(lines[1]!, /^\[C\] sandbox       off for this session/);
	assert.doesNotMatch(lines.join("\n"), /acme-prod/);
	assert.match(lines[2]!, /^Cloud: Off/);
});

test("CA-AWARE-3: an unavailable identity degrades to the panel's own wording", () => {
	setMachineState({ cwd: "/repo", linked: true, workspaceName: undefined, connectionCount: 0, cloudEnabled: true });
	assert.match(machineBarLines(plain)[1]!, new RegExp(`workspace ${IDENTITY_UNKNOWN.replace(/[()]/g, "\\$&")}`));
	setMachineState({ linked: false });
	assert.match(machineBarLines(plain)[1]!, /not linked - cloud tools unavailable/);
});

test("CA-AWARE-4: the legend is drawn once per session and sessions never share an owner", () => {
	const parent = createLegend();
	const child = createLegend();
	assert.equal(parent.tag("call-1"), TAG_LEGEND);
	assert.equal(parent.tag("call-1"), TAG_LEGEND);
	assert.equal(parent.tag("call-2"), undefined);
	assert.equal(child.tag("child-call-1"), TAG_LEGEND, "a child draws its own legend");
	assert.equal(parent.tag("call-2"), undefined, "the child never claims the parent's calls");
	parent.reset();
	assert.equal(parent.tag("call-3"), TAG_LEGEND);
	assert.equal(child.tag("child-call-1"), TAG_LEGEND, "resetting one session leaves the other alone");
});

test("CA-AWARE-5: a tag is ASCII, adds the legend above the call, and shrinks the inner width", () => {
	let seenWidth = 0;
	const inner: Component = {
		render: (width) => {
			seenWidth = width;
			return ["read src/app.ts"];
		},
		invalidate: () => {},
	};
	const tagged = taggedComponent(LOCAL_TAG, theme, inner, TAG_LEGEND);
	const lines = tagged.render(80).map(stripVTControlCharacters);
	assert.deepEqual(lines, [TAG_LEGEND, "[L] read src/app.ts"]);
	assert.equal(seenWidth, 76);
	assert.ok(lines.every((line) => /^[\x20-\x7e]*$/.test(line)));
});

test("CA-AWARE-6: only a missing-path message earns the sandbox clarification, and only once", () => {
	assert.equal(localMissingFile("ENOENT: no such file or directory"), true);
	assert.equal(localMissingFile("File not found: /repo/src/app.ts"), true);
	assert.equal(localMissingFile("Path not found: /repo/src"), true);
	assert.equal(localMissingFile("Error: Cannot find module './missing.js'"), true);
	assert.equal(localMissingFile("exit code 1"), false);
	assert.equal(localMissingFile("bash: line 1: nosuchcmd: command not found"), false);
	assert.equal(localMissingFile("npm ERR! 404 Not Found - GET https://registry.example.com/x"), false);
	assert.equal(localMissingFile("error TS2304: Cannot find name 'foo'."), false);
	assert.equal(localMissingFile("That path was looked for on this machine; missing"), false);
});

test("CA-AWARE-7: untrusted cwd and workspace text cannot carry escapes or extra lines into the bar", () => {
	const evil = "\u001b]0;pwned\u0007/tmp/ev\u001b[31mil\n[L] fake legend\n\r\u001b[2J\u001b[HCloud: Off";
	setMachineState({ cwd: evil, linked: true, workspaceName: "ac\u001b]8;;http://evil\u0007me", connectionCount: 1, cloudEnabled: true });
	const lines = machineBarLines(plain);
	assert.equal(lines.length, 3);
	for (const line of lines) {
		assert.doesNotMatch(line, /\u001b|\r|\n|pwned/);
	}
	assert.match(lines[0]!, /\/tmp\/evil/);
	assert.match(lines[1]!, /workspace acme/);
});

test("CA-AWARE-14: zero-width and bidi format characters never reach a drawn surface", () => {
	const hidden = "\u200b\u200c\u200d\u2060\ufeff\u202e\u061c";
	setMachineState({ cwd: `/tmp/ac${hidden}me`, linked: true, workspaceName: `work${hidden}space`, connectionCount: 1, cloudEnabled: true });
	const lines = machineBarLines(plain);
	for (const line of lines) {
		assert.doesNotMatch(line, /[\u061c\u180e\u200b-\u200f\u202a-\u202e\u2060-\u2064\u2066-\u2069\ufeff]/);
	}
	assert.match(lines[0]!, /\/tmp\/acme/);
	assert.match(lines[1]!, /workspace workspace/);
});
