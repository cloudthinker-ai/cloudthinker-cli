import assert from "node:assert/strict";
import test from "node:test";
import { stripVTControlCharacters } from "node:util";

import { initTheme } from "@earendil-works/pi-coding-agent";
import type { ExtensionAPI, ToolDefinition } from "@earendil-works/pi-coding-agent";

import type { CloudThinkerClient } from "../src/client.ts";
import { TAG_LEGEND } from "../src/awareness.ts";
import { MEMORY_DIR } from "../src/memory.ts";
import { CloudThinkerRuntime } from "../src/runtime.ts";
import { NO_OUTPUT, registerSandboxRead, renderExecution } from "../src/tools/ct-sandbox-read.ts";
import { CT_SANDBOX_READ } from "../src/tools/names.ts";
import { hostVersionsFrom } from "../src/versions.ts";

function readTool(): ToolDefinition {
	const tools = new Map<string, ToolDefinition>();
	const runtime = new CloudThinkerRuntime(
		{
			registerTool: (tool: ToolDefinition) => tools.set(tool.name, tool),
			appendEntry: () => {},
		} as unknown as ExtensionAPI,
		{} as CloudThinkerClient,
		hostVersionsFrom({ version: "0.4.0", piVersion: "0.85.1" }, "0.4.0"),
	);
	registerSandboxRead(runtime);
	const tool = tools.get(CT_SANDBOX_READ);
	assert.ok(tool);
	return tool;
}

test("the description names the workspace machine as the workspace's own durable machine", () => {
	const { description, promptSnippet } = readTool();
	assert.ok(description.includes("the workspace's own machine in the cloud"));
	assert.ok(description.includes("durable machine the whole workspace shares, not a scratch shell"));
	assert.ok(!description.includes("Sandbox"));
	assert.ok(promptSnippet?.includes("workspace machine in the cloud"));
});

test("the description says a Sandbox-only read needs no Connection", () => {
	const { description } = readTool();
	assert.ok(description.includes(`memory tree at ${MEMORY_DIR}`));
	assert.ok(description.includes("Pass an empty connection_list"));
});

const theme = { fg: (_color: string, value: string) => value, bold: (value: string) => value } as never;

function callLines(params: Record<string, unknown>, expanded: boolean): string[] {
	const tool = readTool();
	assert.ok(tool.renderCall);
	return tool.renderCall(params, theme, { expanded } as never).render(200);
}

test("the schema requires a reasoning the user reads in place of the script", () => {
	const { parameters } = readTool();
	const schema = parameters as { required?: string[]; properties: Record<string, { description?: string }> };
	assert.ok(schema.required?.includes("reasoning"));
	assert.match(schema.properties.reasoning?.description ?? "", /in place of the raw command/);
});

test("a successful run renders stdout alone", () => {
	const body = renderExecution({
		status: "completed",
		return_code: 0,
		stdout: '{"count": 3}\n',
		stderr: "warning: deprecated flag\n",
	});
	assert.equal(body, '{"count": 3}');
});

test("a successful run with nothing on stdout falls back to stderr, then to a marker", () => {
	const quiet = { status: "completed", return_code: 0, stdout: "", stderr: "" } as const;
	assert.equal(renderExecution({ ...quiet, stderr: "only on stderr\n" }), "only on stderr");
	assert.equal(renderExecution(quiet), NO_OUTPUT);
});

test("a failed run leads with the exit code, then the error, then whatever stdout carried", () => {
	const body = renderExecution({
		status: "completed",
		return_code: 2,
		stdout: "partial listing\n",
		stderr: "An error occurred (AccessDenied)\n",
	});
	assert.equal(body, "exit code 2\nAn error occurred (AccessDenied)\npartial listing");
	assert.equal(
		renderExecution({ status: "completed", return_code: 1, stdout: "", stderr: "" }),
		"exit code 1",
	);
});

test("the call line leads with the [C] tag and carries no cloud glyph", () => {
	const params = {
		connection_list: ["grafana"],
		reasoning: "Checking which alerts are firing.",
		script: "kubectl get pods -A",
	};
	const [line] = callLines(params, false);
	assert.match(line ?? "", /^\[C\] ct_sandbox_read/);
	assert.ok(!line?.includes("☁"));
});

test("the call line shows the reasoning, and the whole script only when expanded", () => {
	const params = {
		connection_list: ["grafana"],
		reasoning: "Checking which alerts are firing.",
		script: "cat > tmp/q.ts <<'TS'\nconsole.log(1)\nTS\nbun run tmp/q.ts",
	};
	const collapsed = callLines(params, false);
	assert.equal(collapsed.length, 1);
	assert.match(collapsed[0] ?? "", /ct_sandbox_read {2}grafana {2}Checking which alerts are firing\./);
	assert.ok(!collapsed[0]?.includes("cat >"));
	const expanded = callLines(params, true);
	assert.deepEqual(
		expanded.slice(1).map((line) => line.trimEnd()),
		["cat > tmp/q.ts <<'TS'", "console.log(1)", "TS", "bun run tmp/q.ts"],
	);
});

test("a call still streaming without its reasoning falls back to the script's first line", () => {
	const [line] = callLines({ connection_list: [], script: "ls /home/user\nls /tmp" }, false);
	assert.match(line ?? "", /ct_sandbox_read {2}ls \/home\/user$/);
});

test("the first tagged cloud call carries the one legend", () => {
	const tool = readTool();
	assert.ok(tool.renderCall);
	const lines = tool.renderCall(
		{ connection_list: ["grafana"], reasoning: "Checking which alerts are firing.", script: "ls" },
		theme,
		{ expanded: false, toolCallId: "call-1" } as never,
	).render(200).map((line) => line.trimEnd());
	assert.equal(lines[0], TAG_LEGEND);
	assert.match(lines[1] ?? "", /^\[C\] ct_sandbox_read/);
});

function resultLines(expanded: boolean, isError: boolean): string[] {
	initTheme("dark");
	const tool = readTool();
	assert.ok(tool.renderResult);
	const output = Array.from({ length: 8 }, (_, index) => `row ${index + 1}`).join("\n");
	return tool
		.renderResult(
			{ content: [{ type: "text", text: output }], details: { elapsed_ms: 1100 } } as never,
			{ expanded, isPartial: false },
			theme,
			{ isError } as never,
		)
		.render(200)
		.map((line) => stripVTControlCharacters(line).trimEnd());
}

test("a collapsed result hides the output behind the expand hint", () => {
	const lines = resultLines(false, false);
	assert.equal(lines.length, 2);
	assert.equal(lines[0], "ran on the workspace machine · 1.1s");
	assert.match(lines[1] ?? "", /^\(8 lines, .* to expand\)$/);
});

test("an expanded result shows the whole output", () => {
	const lines = resultLines(true, false);
	assert.ok(lines.includes("row 1"));
	assert.ok(lines.includes("row 8"));
});

test("a collapsed error still previews its last lines", () => {
	const lines = resultLines(false, true);
	assert.ok(lines.includes("row 8"));
	assert.ok(!lines.includes("row 1"));
});
