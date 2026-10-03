import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import test from "node:test";

import { AGENT_HELP, MODEL_REFUSAL, NO_SESSION_REFUSAL, PRINT_NEEDS_PROMPT, THINKING_REFUSAL, UPDATE_REFUSAL, checkSurface } from "../src/surface.ts";

test("the wrapper's local review argv still runs", () => {
	const review = ["--models", "cloudthinker/*", "--tools", "read,grep,find,ls", "--no-extensions", "--no-skills", "--no-context-files", "--session-dir", "/tmp/review", "--print", "-p", "Review this checkout"];
	assert.deepEqual(checkSurface(review, false), { kind: "run" });
});

test("the public flags run and --help prints our own help", () => {
	for (const argv of [[], ["fix the build"], ["-p", "hi", "--mode", "json"], ["-c"], ["--model", "pro", "-p", "hi"], ["--model", "cloudthinker/light"], ["--list-models"], ["@notes.md", "summarize"], ["--tools", "read,ls", "-p", "hi"], ["-p", "--", "- a dash prompt"]]) {
		assert.deepEqual(checkSurface(argv, true), { kind: "run" }, argv.join(" "));
	}
	assert.deepEqual(checkSurface(["--model", "pro", "--help"], true), { kind: "help" });
	assert.deepEqual(checkSurface(["-p", "--", "--help"], true), { kind: "run" });
});

test("vendor models, thinking, pi commands, and dropped flags are refused with a next step", () => {
	const refused = (argv: string[], stdin = true) => {
		const result = checkSurface(argv, stdin);
		assert.equal(result.kind, "refuse", argv.join(" "));
		return result.kind === "refuse" ? result.message : "";
	};
	assert.equal(refused(["--provider", "openai", "--model", "gpt-4o"]), MODEL_REFUSAL);
	assert.equal(refused(["--model", "openai/gpt-4o"]), MODEL_REFUSAL);
	assert.equal(refused(["--model", "pro:high"]), MODEL_REFUSAL);
	assert.equal(refused(["--models", "anthropic/*"]), MODEL_REFUSAL);
	assert.equal(refused(["--api-key", "x"]), MODEL_REFUSAL);
	assert.equal(refused(["--thinking", "high"]), THINKING_REFUSAL);
	assert.equal(refused(["--no-session"]), NO_SESSION_REFUSAL);
	assert.equal(refused(["update"]), UPDATE_REFUSAL);
	assert.match(refused(["mcp", "list"]), /not a cloudthinker agent command/);
	assert.match(refused(["--system-prompt", "x"]), /AGENTS\.md/);
	assert.match(refused(["--fork", "abc"]), /\/fork/);
	assert.match(refused(["--mode", "rpc"]), /json or text/);
	assert.match(refused(["--made-up"]), /--made-up is not available/);
	assert.equal(refused(["-p"]), PRINT_NEEDS_PROMPT);
	assert.deepEqual(checkSurface(["-p"], false), { kind: "run" });
});

test("the wrapper prints the same help without starting the agent", () => {
	const copies = ["../../../../cli/crates/cloudthinker-cli/src/agent_help.txt", "../../../../crates/cloudthinker-cli/src/agent_help.txt"].map((path) => new URL(path, import.meta.url));
	const copy = copies.find((url) => existsSync(url));
	assert.ok(copy, "the wrapper's agent_help.txt is missing");
	assert.equal(readFileSync(copy, "utf8"), AGENT_HELP);
});
