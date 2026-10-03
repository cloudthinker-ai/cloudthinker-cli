import assert from "node:assert/strict";
import test from "node:test";

import { singleFlight, withinBudget } from "../src/async.ts";

test("a call during a run queues exactly one re-run with the latest argument", async () => {
	const seen: number[] = [];
	let release: () => void = () => {};
	const gate = new Promise<void>((resolve) => {
		release = resolve;
	});
	const run = singleFlight(async (value: number) => {
		seen.push(value);
		if (seen.length === 1) await gate;
	});
	const first = run(1);
	await run(2);
	await run(3);
	release();
	await first;
	assert.deepEqual(seen, [1, 3]);
	await run(4);
	assert.deepEqual(seen, [1, 3, 4]);
});

test("a failing run rejects its caller and frees the flight", async () => {
	let calls = 0;
	const run = singleFlight(async () => {
		calls += 1;
		if (calls === 1) throw new Error("boom");
	});
	await assert.rejects(() => run(), /boom/);
	await run();
	assert.equal(calls, 2);
});

test("a slow promise resolves at the budget and a fast one at once", async () => {
	let settle: (value: number) => void = () => {};
	const timer = setTimeout(() => settle(1), 5_000);
	const started = Date.now();
	assert.equal(await withinBudget(new Promise<number>((resolve) => { settle = resolve; }), 20), undefined);
	assert.ok(Date.now() - started >= 15);
	settle(1);
	clearTimeout(timer);

	const quick = Date.now();
	assert.equal(await withinBudget(Promise.resolve("done"), 5_000), "done");
	assert.ok(Date.now() - quick < 1_000);
});
