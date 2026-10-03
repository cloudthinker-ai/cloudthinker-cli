import assert from "node:assert/strict";
import test from "node:test";
import { createPhaseTimer } from "../src/timing.ts";

test("CA-AD-7 disabled timing does not read the clock or write output", () => {
	const timer = createPhaseTimer(false, () => { throw new Error("clock read"); }, () => { throw new Error("output"); });
	timer("ignored");
});

test("CA-AD-7 enabled timing emits cumulative and per-phase durations", () => {
	const lines: string[] = [];
	let now = 12;
	const timer = createPhaseTimer(true, () => now, (line) => lines.push(line));
	timer("agent.modules");
	now = 19;
	timer("agent.models");
	assert.deepEqual(lines.map((line) => JSON.parse(line.slice(line.indexOf("{")))), [
		{ phase: "agent.modules", elapsed_ms: 12, phase_ms: 12 },
		{ phase: "agent.models", elapsed_ms: 19, phase_ms: 7 },
	]);
});
