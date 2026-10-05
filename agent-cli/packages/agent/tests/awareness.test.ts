import assert from "node:assert/strict";
import test from "node:test";
import { stripVTControlCharacters } from "node:util";
import type { ToolDefinition } from "@earendil-works/pi-coding-agent";
import { Text } from "@earendil-works/pi-tui";


import { clickToExpand, pointer, tagLocalToolDefinition } from "../src/awareness.ts";
import { splitCommand } from "../src/command-block.ts";
import { parsePiDiff, renderHunks } from "../src/diff-view.ts";
import { setDiffStyle, setToolOutputMode } from "../src/verbosity.ts";

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

const editDiff = [
	" 10 const a = 1;",
	" 11 const b = 2;",
	"-12 const total = first + second;",
	"-13 const label = 'old';",
	"+12 const total = first + second + third;",
	"+13 const label = 'new';",
	"+14 const extra = true;",
	" 15 const c = 3;",
	...Array.from({ length: 30 }, (_, index) => `+${16 + index} added line ${index}`),
].join("\n");

function renderEdit(width: number, context: object): string[] {
	const upstream = { name: "edit", renderCall: () => new Text("edit app.ts", 0, 0) } as unknown as ToolDefinition;
	const wrapped = tagLocalToolDefinition("edit", upstream)!;
	const fullTheme = { ...(theme as object), bg: (_color: string, value: string) => value, inverse: (value: string) => value } as never;
	const state = { callComponent: { preview: { diff: editDiff } } };
	return wrapped.renderCall!({}, fullTheme, { toolCallId: "e1", state, cwd: "/tmp", ...context } as never).render(width).map(stripVTControlCharacters).map((line) => line.trimEnd());
}

test("an expanded edit is split when each pane gets 60 columns and unified below, with paired rows and aligned gutters", () => {
	setDiffStyle("auto");
	const narrow = renderEdit(80, { expanded: true }).join("\n");
	assert.match(narrow, /╌╌ line 10 ╌+/);
	assert.match(narrow, /\n\s+12 - const total = first \+ second;\n\s+13 - const label = 'old';\n\s+12 \+ const total = first \+ second \+ third;/);
	assert.doesNotMatch(narrow, /│/);

	const wide = renderEdit(160, { expanded: true });
	const pair = wide.find((line) => line.includes("const total = first + second;"))!;
	assert.match(pair, /^\s+12 - const total = first \+ second;\s+│\s+12 \+ const total = first \+ second \+ third;$/);
	const filler = wide.find((line) => line.includes("const extra = true;"))!;
	assert.match(filler, /^\s+│\s+14 \+ const extra = true;$/);
	const columns = wide.filter((line) => line.includes("│")).map((line) => line.indexOf("│"));
	assert.equal(new Set(columns).size, 1);

	const content = "import { describe } from './cart.ts';\nconst enabled = true;\n";
	const write = tagLocalToolDefinition("write", { name: "write", renderCall: () => new Text("write", 0, 0) } as unknown as ToolDefinition)!;
	const written = write.renderCall!({ path: "new.ts", content }, { ...(theme as object), bg: (_c: string, v: string) => v } as never, { toolCallId: "w1", expanded: true, state: {}, cwd: "/tmp" } as never).render(170).map(stripVTControlCharacters).map((line) => line.trimEnd()).join("\n");
	assert.match(written, /╌╌ 2 lines written ╌+\n1 \+ import/);
	assert.doesNotMatch(written, /│/);

	const wrapped = renderEdit(36, { expanded: true });
	const start = wrapped.findIndex((line) => line.startsWith(" 12 + const total"));
	assert.deepEqual(wrapped.slice(start, start + 3), [" 12 + const total = first + second", "      + third;", " 13 + const label = 'new';"]);

	setDiffStyle("unified");
	assert.doesNotMatch(renderEdit(160, { expanded: true }).join("\n"), /│/);
	setDiffStyle("split");
	assert.match(renderEdit(130, { expanded: true }).join("\n"), /│/);
	setDiffStyle("auto");

	const plain = { fg: (_color: string, value: string) => value, inverse: (value: string) => `[${value}]` };
	const respaced = renderHunks(parsePiDiff("-1 foo  bar baz\n+1 foo bar qux"), 80, plain, "unified").join("\n");
	assert.match(respaced, /1 - foo\[  \]bar \[baz\]/);
	assert.match(respaced, /1 \+ foo\[ \]bar \[qux\]/);
});

test("preview shows a capped diff with a fold hint, split only when wide, and compact keeps the one-line summary", () => {
	try {
		setToolOutputMode("preview");
		assert.match(renderEdit(160, { expanded: false }).join("\n"), /│/);
		const lines = renderEdit(100, { expanded: false });
		assert.doesNotMatch(lines.join("\n"), /│/);
		assert.equal(lines.filter((line) => /^ (╌╌|\d+ )/.test(line)).length, 12);
		assert.ok(lines.some((line) => /^ … \+\d+ lines · ctrl\+o$/.test(line)));
		setToolOutputMode("compact");
		assert.match(renderEdit(160, { expanded: false }).join("\n"), /\+\d+ −2/);
	} finally {
		setToolOutputMode("compact");
	}
});

test("a shell command splits at top-level operators and newlines, never inside quotes or a heredoc", () => {
	const segments = splitCommand("cd app && sed -i 's/a && b/c; d/' f.txt | tee \"x || y\"; cat <<'EOF' > out.txt\nline && one\nline | two\nEOF\necho done");
	assert.deepEqual(segments.map((segment) => segment.text), [
		"cd app",
		"&& sed -i 's/a && b/c; d/' f.txt",
		"| tee \"x || y\"",
		"; cat <<'EOF' > out.txt",
		"echo done",
	]);
	assert.deepEqual(segments[3]!.body, ["line && one", "line | two"]);

	const wrapped = tagLocalToolDefinition("bash", definition("bash", "$ x"))!;
	const command = "git add . && git commit -m 'one && two'\ncat <<EOF\n1\n2\n3\n4\n5\n6\nEOF";
	const render = (expanded: boolean) =>
		wrapped.renderCall!({ command }, theme, { toolCallId: "b1", expanded, state: {} } as never).render(60).map(stripVTControlCharacters);
	try {
		setToolOutputMode("preview");
		assert.deepEqual(render(false), [
			"[L] $ git add .",
			"      │ && git commit -m 'one && two'",
			"      │ cat <<EOF",
			"      │ 1",
			"      │ 2",
			"      │ 3",
			"      │ 4",
			"      │ … +2 lines",
			"      │ EOF",
		].map((line, index) => (index === 0 ? line : line.replace("      │", "    │"))));
		assert.equal(render(true).filter((line) => /^\s+│ [1-6]$/.test(line)).length, 6);
		setToolOutputMode("compact");
		assert.deepEqual(render(false), ["[L] $ git add . && git commit -m 'one && two' +8 lines"]);
	} finally {
		setToolOutputMode("compact");
	}
});
