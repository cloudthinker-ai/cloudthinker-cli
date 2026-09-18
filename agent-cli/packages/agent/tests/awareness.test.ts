import assert from "node:assert/strict";
import test from "node:test";
import { stripVTControlCharacters } from "node:util";
import type { ToolDefinition } from "@earendil-works/pi-coding-agent";
import { Text } from "@earendil-works/pi-tui";

import { TAG_LEGEND, createLegend } from "@cloudthinker/pi/src/awareness.ts";

import { tagLocalToolDefinition } from "../src/awareness.ts";

const theme = { fg: (_color: string, value: string) => value, bold: (value: string) => value } as never;

function definition(name: string, line: string): ToolDefinition {
	return {
		name,
		renderCall: () => ({ render: () => [line], invalidate: () => {} }),
	} as unknown as ToolDefinition;
}

test("CA-AWARE-7: a local call renders under [L] with the one legend, and unknown tools are untouched", () => {
	const legend = createLegend();
	const tagged = tagLocalToolDefinition("bash", definition("bash", "$ git status"), legend)!;
	const lines = tagged.renderCall!({}, theme, { toolCallId: "call-1" } as never).render(80).map(stripVTControlCharacters);
	assert.deepEqual(lines, [TAG_LEGEND, "[L] $ git status"]);
	const second = tagLocalToolDefinition("read", definition("read", "read a.ts"), legend)!;
	const secondLines = second.renderCall!({}, theme, { toolCallId: "call-2" } as never).render(80).map(stripVTControlCharacters);
	assert.deepEqual(secondLines, ["[L] read a.ts"]);
	const shell = tagLocalToolDefinition("powershell", definition("powershell", "Get-ChildItem"), legend)!;
	const shellLines = shell.renderCall!({}, theme, { toolCallId: "call-3" } as never).render(80).map(stripVTControlCharacters);
	assert.deepEqual(shellLines, ["[L] Get-ChildItem"]);
});

test("CA-AWARE-8: a cloud or non-built-in tool is never retagged, and a definition without a renderer is returned as is", () => {
	const cloud = definition("ct_ask", "[C] ct_ask hello");
	assert.equal(tagLocalToolDefinition("ct_ask", cloud), cloud);
	assert.equal(tagLocalToolDefinition("Agent", cloud), cloud);
	assert.equal(tagLocalToolDefinition("bash", { name: "bash" } as ToolDefinition)?.name, "bash");
	assert.equal(tagLocalToolDefinition("bash", undefined), undefined);
});

test("CA-AWARE-12: a renderer that reuses pi's lastComponent keeps its text across re-renders", () => {
	const upstream: ToolDefinition = {
		name: "bash",
		renderCall: (args: any, _theme: any, context: any) => {
			const text = context.lastComponent ?? new Text("", 0, 0);
			text.setText(`$ ${args.command}`);
			return text;
		},
	} as unknown as ToolDefinition;
	const wrapped = tagLocalToolDefinition("bash", upstream, createLegend())!;
	assert.ok(wrapped.renderCall);
	const first = wrapped.renderCall({ command: "git status" }, theme, { toolCallId: "call-9" } as never);
	const second = wrapped.renderCall({ command: "git status --short" }, theme, { toolCallId: "call-9", lastComponent: first } as never);
	assert.deepEqual(
		second.render(80).map(stripVTControlCharacters).map((line) => line.trimEnd()),
		[TAG_LEGEND, "[L] $ git status --short"],
	);
	assert.match(
		first.render(80).map(stripVTControlCharacters).map((line) => line.trimEnd()).join("\n"),
		/\$ git status --short$/,
	);
});
