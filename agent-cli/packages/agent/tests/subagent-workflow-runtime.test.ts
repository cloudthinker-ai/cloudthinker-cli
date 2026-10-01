import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import test from "node:test";
import { ModuleKind, ScriptTarget, transpileModule } from "typescript";

import { runWorkflow, workflowConcurrency } from "@tintinweb/pi-subagents/dist/workflow/runtime.js";

async function compileWorkflowRuntimeSource() {
	const distWorkflow = dirname(fileURLToPath(import.meta.resolve("@tintinweb/pi-subagents/dist/workflow/runtime.js")));
	const sourcePath = join(distWorkflow, "../../src/workflow/runtime.ts");
	const source = readFileSync(sourcePath, "utf8");
	const output = transpileModule(source, { compilerOptions: { module: ModuleKind.ESNext, target: ScriptTarget.ES2022 } }).outputText
		.replace(/from ["']\.\/([^"']+\.js)["']/g, (_match, file: string) => `from ${JSON.stringify(pathToFileURL(join(distWorkflow, file)).href)}`);
	const directory = mkdtempSync(join(tmpdir(), "ct-workflow-source-"));
	const modulePath = join(directory, "runtime.mjs");
	writeFileSync(modulePath, output);
	return { module: await import(pathToFileURL(modulePath).href), cleanup: () => rmSync(directory, { recursive: true, force: true }) };
}

test("workflow concurrency defaults to eight, validates overrides, and shares the limit with nested work", { timeout: 10_000 }, async () => {
	const previous = process.env.CLOUDTHINKER_WORKFLOW_MAX_CONCURRENCY;
	delete process.env.CLOUDTHINKER_WORKFLOW_MAX_CONCURRENCY;
	assert.equal(workflowConcurrency(64), 8);
	assert.equal(workflowConcurrency(3), 1);
	process.env.CLOUDTHINKER_WORKFLOW_MAX_CONCURRENCY = "4";
	assert.equal(workflowConcurrency(64), 4);
	process.env.CLOUDTHINKER_WORKFLOW_MAX_CONCURRENCY = "8";
	assert.equal(workflowConcurrency(64), 8);
	let rejectedSpawns = 0;
	for (const invalid of ["0", "9", "1.5", "nope"]) {
		process.env.CLOUDTHINKER_WORKFLOW_MAX_CONCURRENCY = invalid;
		assert.throws(() => workflowConcurrency(64), /CLOUDTHINKER_WORKFLOW_MAX_CONCURRENCY must be an integer from 1 to 8/);
		await assert.rejects(runWorkflow({
			script: 'export const meta = { name: "invalid concurrency", description: "Do not dispatch" }; return await agent("unreachable");',
			host: {
				async spawnAgent() { rejectedSpawns++; return { ok: true, text: "unexpected" }; },
				abortAgent() {},
			},
		}), /CLOUDTHINKER_WORKFLOW_MAX_CONCURRENCY must be an integer from 1 to 8/);
	}
	assert.equal(rejectedSpawns, 0);
	process.env.CLOUDTHINKER_WORKFLOW_MAX_CONCURRENCY = "4";
	let active = 0;
	let maximum = 0;
	const nested = `export const meta = { name: "nested", description: "Nested work" }; return await parallel(Array.from({ length: 6 }, (_, index) => () => agent("nested " + index)));`;
	const script = `export const meta = { name: "root", description: "Root work" }; const root = parallel(Array.from({ length: 6 }, (_, index) => () => agent("root " + index))); const child = workflow("nested"); return await Promise.all([root, child]);`;
	const result = await runWorkflow({
		script,
		host: {
			async spawnAgent(request) {
				active++;
				maximum = Math.max(maximum, active);
				await new Promise((resolve) => setTimeout(resolve, 5));
				active--;
				return { ok: true, text: request.prompt };
			},
			abortAgent() {},
			async loadWorkflow() {
				return { ok: true, script: nested };
			},
		},
	});
	assert.equal(result.status, "completed");
	assert.equal(result.agentCount, 12);
	assert.ok(maximum <= 4, `observed ${maximum} concurrent agents`);
	if (previous === undefined) delete process.env.CLOUDTHINKER_WORKFLOW_MAX_CONCURRENCY;
	else process.env.CLOUDTHINKER_WORKFLOW_MAX_CONCURRENCY = previous;
});

test("source and dist workflow runtimes agree on concurrency and JSON boundary behavior", async () => {
	const previous = process.env.CLOUDTHINKER_WORKFLOW_MAX_CONCURRENCY;
	const compiledSource = await compileWorkflowRuntimeSource();
	try {
		for (const runtime of [compiledSource.module, { workflowConcurrency, runWorkflow }]) {
			delete process.env.CLOUDTHINKER_WORKFLOW_MAX_CONCURRENCY;
			assert.equal(runtime.workflowConcurrency(64), 8);
			process.env.CLOUDTHINKER_WORKFLOW_MAX_CONCURRENCY = "4";
			assert.equal(runtime.workflowConcurrency(64), 4);
			process.env.CLOUDTHINKER_WORKFLOW_MAX_CONCURRENCY = "8";
			assert.equal(runtime.workflowConcurrency(64), 8);
			for (const invalid of ["0", "9", "1.5", "nope"]) {
				process.env.CLOUDTHINKER_WORKFLOW_MAX_CONCURRENCY = invalid;
				assert.throws(() => runtime.workflowConcurrency(64), /CLOUDTHINKER_WORKFLOW_MAX_CONCURRENCY must be an integer from 1 to 8/);
			}
			process.env.CLOUDTHINKER_WORKFLOW_MAX_CONCURRENCY = "4";
			const host = {
				async spawnAgent() { throw new Error("No agent should be dispatched"); },
				abortAgent() {},
				async loadWorkflow() { return { ok: false as const, message: "No nested workflow expected" }; },
			};
			const malformed = await runtime.runWorkflow({
				script: 'export const meta = { name: "source parity bad args", description: "Reject nested undefined" }; return { run: args.run };',
				args: JSON.stringify({ run: "fixture" }),
				host,
			});
			assert.equal(malformed.status, "failed");
			assert.match(malformed.error ?? "", /Cannot pass undefined across the workflow VM boundary \(at the workflow result\.run\)/);
			const valid = await runtime.runWorkflow({
				script: 'export const meta = { name: "source parity good args", description: "Preserve object args" }; return { run: args.run };',
				args: { run: "fixture" },
				host,
			});
			assert.equal(valid.status, "completed");
			assert.deepEqual(valid.value, { run: "fixture" });
		}
	} finally {
		compiledSource.cleanup();
		if (previous === undefined) delete process.env.CLOUDTHINKER_WORKFLOW_MAX_CONCURRENCY;
		else process.env.CLOUDTHINKER_WORKFLOW_MAX_CONCURRENCY = previous;
	}
});

test("workflow results keep rejecting nested undefined while accepting top-level undefined", async () => {
	const host = {
		async spawnAgent() {
			throw new Error("No agent should be dispatched");
		},
		abortAgent() {},
		async loadWorkflow() {
			return { ok: false as const, message: "No nested workflow expected" };
		},
	};
	const malformed = await runWorkflow({
		script: 'export const meta = { name: "malformed args", description: "Expose bad args" }; return { run: args.run };',
		args: JSON.stringify({ run: "fixture" }),
		host,
	});
	assert.equal(malformed.status, "failed");
	assert.match(malformed.error ?? "", /Cannot pass undefined across the workflow VM boundary \(at the workflow result\.run\)/);
	assert.equal(malformed.agentCount, 0);

	const valid = await runWorkflow({
		script: 'export const meta = { name: "valid args", description: "Preserve object args" }; return { run: args.run };',
		args: { run: "fixture" },
		host,
	});
	assert.equal(valid.status, "completed");
	assert.deepEqual(valid.value, { run: "fixture" });

	const noResult = await runWorkflow({
		script: 'export const meta = { name: "empty result", description: "Allow no output" }; return undefined;',
		host,
	});
	assert.equal(noResult.status, "completed");
	assert.equal(noResult.value, undefined);
});
