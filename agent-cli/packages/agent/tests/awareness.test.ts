import assert from "node:assert/strict";
import test from "node:test";
import { stripVTControlCharacters } from "node:util";
import type { ToolDefinition } from "@earendil-works/pi-coding-agent";
import { Text } from "@earendil-works/pi-tui";


import { clickToExpand, pointer, tagLocalToolDefinition } from "../src/awareness.ts";

const theme = { fg: (_color: string, value: string) => value, bold: (value: string) => value } as never;

function definition(name: string, line: string): ToolDefinition {
	return {
		name,
		renderCall: () => ({ render: () => [line], invalidate: () => {} }),
	} as unknown as ToolDefinition;
}

test("CA-AWARE-7: a local call renders under [L] with no legend line, and unknown tools are untouched", () => {
	const tagged = tagLocalToolDefinition("bash", definition("bash", "$ git status"))!;
	const lines = tagged.renderCall!({}, theme, { toolCallId: "call-1" } as never).render(80).map(stripVTControlCharacters);
	assert.deepEqual(lines, ["[L] $ git status"]);
	const second = tagLocalToolDefinition("read", definition("read", "read a.ts"))!;
	const secondLines = second.renderCall!({}, theme, { toolCallId: "call-2" } as never).render(80).map(stripVTControlCharacters);
	assert.deepEqual(secondLines, ["[L] read a.ts"]);
	const shell = tagLocalToolDefinition("powershell", definition("powershell", "Get-ChildItem"))!;
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
	const wrapped = tagLocalToolDefinition("bash", upstream)!;
	assert.ok(wrapped.renderCall);
	const first = wrapped.renderCall({ command: "git status" }, theme, { toolCallId: "call-9" } as never);
	const second = wrapped.renderCall({ command: "git status --short" }, theme, { toolCallId: "call-9", lastComponent: first } as never);
	assert.deepEqual(
		second.render(80).map(stripVTControlCharacters).map((line) => line.trimEnd()),
		["[L] $ git status --short"],
	);
	assert.match(
		first.render(80).map(stripVTControlCharacters).map((line) => line.trimEnd()).join("\n"),
		/\$ git status --short$/,
	);
});

test("compact tool output folds a running or successful local result to one line and keeps errors and expand", async () => {
	const { setToolOutputMode } = await import("../src/verbosity.ts");
	const seen: unknown[] = [];
	const upstream = {
		name: "bash",
		renderCall: () => new Text("$ rg -n class", 0, 0),
		renderResult: (_result: any, _options: any, _theme: any, context: any) => {
			seen.push(context.lastComponent);
			return new Text("line 1\nline 2\nline 3", 0, 0);
		},
	} as unknown as ToolDefinition;
	const wrapped = tagLocalToolDefinition("bash", upstream)!;
	const result = { content: [{ type: "text", text: "a\nb\nc\n" }] };
	const state = { startedAt: 1_000, endedAt: 1_100 };
	const render = (options: object, context: object = {}) =>
		wrapped.renderResult!(result as never, { expanded: false, isPartial: false, ...options } as never, theme, { state, ...context } as never)
			.render(80).map(stripVTControlCharacters).map((line) => line.trim());
	try {
		setToolOutputMode("compact");
		assert.deepEqual(render({}), ["3 lines · 0.1s"]);
		assert.deepEqual(render({ expanded: true }), ["line 1", "line 2", "line 3"]);
		assert.deepEqual(render({}, { isError: true }), ["line 1", "line 2", "line 3"]);
		assert.deepEqual(render({ isPartial: true }), ["⋯ running · 3 lines · ctrl+b to background"]);
		assert.deepEqual(render({ isPartial: true }, { args: { command: "sleep 30" } }), ["⋯ running · 3 lines"]);
		const moved = wrapped.renderResult!({ ...result, details: { backgroundTaskId: "task-1" } } as never, { expanded: false, isPartial: false } as never, theme, { state } as never);
		assert.deepEqual(moved.render(80).map(stripVTControlCharacters).map((line) => line.trim()), ["⇢ moved to background · task-1"]);
		const stopped = wrapped.renderResult!({ content: [{ type: "text", text: "a\nb\n\nCommand aborted" }] } as never, { expanded: false, isPartial: false } as never, theme, { state, isError: true } as never);
		assert.deepEqual(stopped.render(80).map(stripVTControlCharacters).map((line) => line.trim()), ["stopped · 2 lines · 0.1s"]);
		const call = wrapped.renderCall!({ command: "echo one\necho two\necho three" } as never, theme, { toolCallId: "call-2" } as never);
		assert.equal(stripVTControlCharacters(call.render(80).at(-1) ?? "").trimEnd(), "[L] $ echo one +2 lines");
		const long = wrapped.renderCall!({ command: `du -sh ${"dir ".repeat(40)}` } as never, theme, { toolCallId: "call-3" } as never);
		assert.equal(long.render(40).length, 1);
		const multiLong = wrapped.renderCall!({ command: `du -sh ${"dir ".repeat(40)}\necho done` } as never, theme, { toolCallId: "call-4" } as never);
		const [row] = multiLong.render(60);
		assert.ok(!row!.includes("\x1b[0m"), "a full reset drops the tool box background behind the ellipsis and +N lines");
		assert.match(stripVTControlCharacters(row!).trimEnd(), /^\[L\] \$ du -sh dir .*\.\.\. \+1 lines$/);
		assert.deepEqual(render({ isPartial: true, expanded: true }), ["line 1", "line 2", "line 3"]);
		assert.ok(seen.slice(1).every((component) => component instanceof Text && component.render(80).join("\n").includes("line 1")));
		setToolOutputMode("preview");
		assert.deepEqual(render({}), ["line 1", "line 2", "line 3"]);
		assert.equal(tagLocalToolDefinition("edit", { ...upstream, name: "edit" } as ToolDefinition)!.renderResult, upstream.renderResult);
	} finally {
		setToolOutputMode("compact");
	}
});

test("pi's leftover expand hint points at a click in fullscreen and at the transcript where there is no mouse", () => {
	const hinted = clickToExpand({ render: () => ["... (11 earlier lines, \x1b[2mctrl+o\x1b[0m\x1b[90m to expand)"], invalidate: () => {} });
	assert.equal(stripVTControlCharacters(hinted.render(80)[0]!), "... (11 earlier lines, click to expand)");
	pointer.available = false;
	assert.equal(stripVTControlCharacters(hinted.render(80)[0]!), "... (11 earlier lines, ctrl+o for the transcript)");
	pointer.available = true;
});
