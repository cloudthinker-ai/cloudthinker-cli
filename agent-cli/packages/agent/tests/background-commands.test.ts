import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { createLocalBashOperations, type BashOperations } from "@earendil-works/pi-coding-agent";
import {
	BACKGROUND_MAX_OUTPUT_BYTES,
	BACKGROUND_MAX_TASKS,
	BackgroundCommandManager,
} from "../src/background/manager.ts";

async function fixture(operations: BashOperations = createLocalBashOperations()) {
	const root = await mkdtemp(join(tmpdir(), "ct-background-"));
	const manager = new BackgroundCommandManager({ storageDirectory: join(root, "private"), cwd: root, operations });
	await manager.initialize();
	return { root, manager };
}

test("background commands preserve success, failure, and byte cursor output", async () => {
	const { root, manager } = await fixture();
	try {
		const success = await manager.start("printf 'hello'; printf ' world' >&2", root);
		await manager.waitForActive();
		assert.equal(manager.get(success.id).state, "succeeded");
		const first = manager.readOutput(success.id, 0, 5);
		assert.equal(first.text, "hello");
		assert.equal(first.nextByte, 5);
		const second = manager.readOutput(success.id, first.nextByte, 100);
		assert.equal(second.text, " world");
		assert.equal(second.totalBytes, 11);
		assert.equal("output" in manager.get(success.id), false);
		assert.equal("output" in success, false);

		const failure = await manager.start("printf 'problem'; exit 7", root);
		await manager.waitForActive();
		assert.equal(manager.get(failure.id).state, "failed");
		assert.equal(manager.get(failure.id).exitCode, 7);
		assert.equal(manager.readOutput(failure.id).text, "problem");
	} finally {
		await manager.shutdown();
		await rm(root, { recursive: true, force: true });
	}
});

test("background command cancellation aborts the process tree and settles once", async () => {
	const root = await mkdtemp(join(tmpdir(), "ct-background-cancel-"));
	const terminals: string[] = [];
	const withEvents = new BackgroundCommandManager({
		storageDirectory: join(root, "events"),
		cwd: root,
		operations: createLocalBashOperations(),
		onTerminal: (task) => { terminals.push(task.id); },
	});
	await withEvents.initialize();
	try {
		const task = await withEvents.start("printf 'ready'; sleep 30", root);
		const cancelled = await withEvents.cancel(task.id);
		assert.equal(cancelled.state, "cancelled");
		assert.deepEqual(terminals, []);
		assert.deepEqual(withEvents.takeCompletions(), []);
		assert.equal(withEvents.get(task.id).state, "cancelled");
	} finally {
		await withEvents.shutdown();
		await rm(root, { recursive: true, force: true });
	}
});

test("background command deadline produces a timed_out terminal state", async () => {
	const { root, manager } = await fixture();
	try {
		const task = await manager.start("sleep 5", root, 0.05);
		await manager.waitForActive();
		assert.equal(manager.get(task.id).state, "timed_out");
	} finally {
		await manager.shutdown();
		await rm(root, { recursive: true, force: true });
	}
});

test("output retention and byte cursors stay bounded and do not split UTF-8", async () => {
	const { root, manager } = await fixture();
	try {
		const large = await manager.start("printf '%0200000d' 0; printf '🙂done'", root);
		await manager.waitForActive();
		const summary = manager.get(large.id);
		assert.equal(summary.totalOutputBytes, 200008);
		assert.ok(summary.totalOutputBytes - manager.readOutput(large.id, 0, 4).startByte <= BACKGROUND_MAX_OUTPUT_BYTES);
		const tail = manager.readOutput(large.id, summary.totalOutputBytes - 6, 6);
		assert.equal(tail.text, "done");
		assert.equal(tail.droppedBytes, 2);
		const missing = manager.readOutput(large.id, 0, 10);
		assert.equal(missing.truncated, true);
		assert.ok(missing.startByte > 0);
		assert.equal(missing.droppedBytes, missing.startByte);
	} finally {
		await manager.shutdown();
		await rm(root, { recursive: true, force: true });
	}
});

test("recovery marks recorded running tasks interrupted without trusting a stored PID", async () => {
	const root = await mkdtemp(join(tmpdir(), "ct-background-recovery-"));
	const pending: { resolve?: (result: { exitCode: number | null }) => void } = {};
	const operations: BashOperations = {
		exec: async (_command, _cwd, { onData }) => {
			onData(Buffer.from("working"));
			return await new Promise((resolve) => { pending.resolve = resolve; });
		},
	};
	const directory = join(root, "private");
	const first = new BackgroundCommandManager({ storageDirectory: directory, cwd: root, operations });
	let second: BackgroundCommandManager | undefined;
	await first.initialize();
	try {
		const task = await first.start("fixture", root);
		const tasksPath = join(directory, "tasks.json");
		const deadline = Date.now() + 2_000;
		let persisted: { tasks: Record<string, unknown>[] };
		for (;;) {
			persisted = JSON.parse(await readFile(tasksPath, "utf8")) as { tasks: Record<string, unknown>[] };
			const recorded = persisted.tasks[0];
			const output = typeof recorded?.output === "string" ? Buffer.from(recorded.output, "base64").toString("utf8") : "";
			if (recorded?.state === "running" && output === "working") break;
			if (Date.now() >= deadline) throw new Error("timed out waiting for persisted background output");
			await new Promise((resolve) => setTimeout(resolve, 10));
		}
		assert.equal(persisted.tasks[0]?.state, "running");
		assert.equal("pid" in (persisted.tasks[0] ?? {}), false);
		pending.resolve?.({ exitCode: 0 });
		await first.waitForActive();
		await first.shutdown();
		await writeFile(join(directory, "tasks.json"), JSON.stringify(persisted));
		second = new BackgroundCommandManager({ storageDirectory: directory, cwd: root, operations });
		await second.initialize();
		assert.equal(second.get(task.id).state, "interrupted");
		assert.equal(second.readOutput(task.id).text, "working");
		assert.match(second.takeRecoveryNotices()[0] ?? "", /interrupted.*fixture/);
		assert.deepEqual(second.takeRecoveryNotices(), []);
		assert.equal((await stat(directory)).mode & 0o777, 0o700);
		assert.equal((await stat(tasksPath)).mode & 0o777, 0o600);
	} finally {
		pending.resolve?.({ exitCode: null });
		await first.shutdown();
		await second?.shutdown();
		await rm(root, { recursive: true, force: true });
	}
});

test("recovery surfaces a terminal result that was persisted before notification", async () => {
	const root = await mkdtemp(join(tmpdir(), "ct-background-recovery-terminal-"));
	const directory = join(root, "private");
	await mkdir(directory, { mode: 0o700 });
	await writeFile(join(directory, "tasks.json"), JSON.stringify({
		version: 1,
		tasks: [{ id: "task-1", command: "printf done", cwd: root, state: "succeeded", createdAt: 1, finishedAt: 2, exitCode: 0, outputBaseByte: 0, totalOutputBytes: 4, output: Buffer.from("done").toString("base64"), completionDelivered: false }],
	}));
	const manager = new BackgroundCommandManager({ storageDirectory: directory, cwd: root, operations: createLocalBashOperations() });
	await manager.initialize();
	try {
		assert.equal(manager.get("task-1").state, "succeeded");
		assert.deepEqual(manager.takeRecoveryNotices(), ["task-1: succeeded — printf done"]);
		assert.deepEqual(manager.takeRecoveryNotices(), []);
	} finally {
		await manager.shutdown();
		await rm(root, { recursive: true, force: true });
	}
});

test("session state lock rejects a second owner and releases on shutdown", async () => {
	const root = await mkdtemp(join(tmpdir(), "ct-background-lock-"));
	const options = { storageDirectory: join(root, "private"), cwd: root, operations: createLocalBashOperations() };
	const first = new BackgroundCommandManager(options);
	const second = new BackgroundCommandManager(options);
	await first.initialize();
	try {
		await assert.rejects(second.initialize(), /already active in another process/);
		await first.shutdown();
		await second.initialize();
	} finally {
		await first.shutdown();
		await second.shutdown();
		await rm(root, { recursive: true, force: true });
	}
});

test("acknowledged cancelled tasks can be pruned at the retention limit", async () => {
	const root = await mkdtemp(join(tmpdir(), "ct-background-cancel-prune-"));
	const operations: BashOperations = {
		exec: async (_command, _cwd, { signal }) => {
			if (signal?.aborted) return { exitCode: null };
			return await new Promise((resolve) => signal?.addEventListener("abort", () => resolve({ exitCode: null }), { once: true }));
		},
	};
	const manager = new BackgroundCommandManager({ storageDirectory: join(root, "private"), cwd: root, operations });
	await manager.initialize();
	try {
		const cancelled: string[] = [];
		for (let index = 0; index < BACKGROUND_MAX_TASKS; index++) {
			const task = await manager.start(`fixture-${index}`, root);
			assert.equal((await manager.cancel(task.id)).state, "cancelled");
			cancelled.push(task.id);
		}
		const next = await manager.start("next", root);
		assert.equal(manager.list().length, BACKGROUND_MAX_TASKS);
		assert.throws(() => manager.get(cancelled[0]!), /Unknown background task/);
		await manager.cancel(next.id);
	} finally {
		await manager.shutdown();
		await rm(root, { recursive: true, force: true });
	}
});

test("completion notices are drained exactly once", async () => {
	const root = await mkdtemp(join(tmpdir(), "ct-background-events-"));
	const notified: string[] = [];
	const manager = new BackgroundCommandManager({
		storageDirectory: join(root, "private"),
		cwd: root,
		operations: createLocalBashOperations(),
		onTerminal: (task) => { notified.push(task.id); },
	});
	await manager.initialize();
	try {
		const task = await manager.start("printf done", root);
		await manager.waitForActive();
		assert.deepEqual(manager.takeCompletions().map((item) => item.id), [task.id]);
		assert.deepEqual(manager.takeCompletions(), []);
		await new Promise((resolve) => setImmediate(resolve));
		assert.deepEqual(notified, [task.id]);
	} finally {
		await manager.shutdown();
		await rm(root, { recursive: true, force: true });
	}
});

test("cancelling a command kills same-process-group descendants", async () => {
	const root = await mkdtemp(join(tmpdir(), "ct-background-descendants-"));
	const manager = new BackgroundCommandManager({
		storageDirectory: join(root, "private"),
		cwd: root,
		operations: createLocalBashOperations(),
	});
	await manager.initialize();
	try {
		const task = await manager.start("sleep 30 & child=$!; printf '%s' \"$child\"; wait", root);
		let childPid = "";
		for (let attempt = 0; attempt < 50 && childPid.length === 0; attempt++) {
			childPid = manager.readOutput(task.id).text;
			if (!childPid) await new Promise((resolve) => setTimeout(resolve, 10));
		}
		assert.match(childPid, /^\d+$/);
		await manager.cancel(task.id);
		assert.throws(() => process.kill(Number(childPid), 0), { code: "ESRCH" });
	} finally {
		await manager.shutdown();
		await rm(root, { recursive: true, force: true });
	}
});

test("background shell cleanup stops same-group descendants after a successful shell exit", async () => {
	const root = await mkdtemp(join(tmpdir(), "ct-background-exit-descendants-"));
	const manager = new BackgroundCommandManager({
		storageDirectory: join(root, "private"),
		cwd: root,
		operations: createLocalBashOperations({ cleanupOnExit: true }),
	});
	await manager.initialize();
	try {
		const task = await manager.start("sleep 30 & child=$!; printf '%s' \"$child\"; exit 0", root);
		await manager.waitForActive();
		assert.equal(manager.get(task.id).state, "succeeded");
		assert.equal(manager.get(task.id).exitCode, 0);
		const pid = Number(manager.readOutput(task.id).text);
		assert.ok(Number.isInteger(pid) && pid > 0);
		let running = true;
		for (let attempt = 0; attempt < 20 && running; attempt++) {
			try {
				process.kill(pid, 0);
				await new Promise((resolve) => setTimeout(resolve, 10));
			} catch (error) {
				if (typeof error === "object" && error !== null && "code" in error && error.code === "ESRCH") running = false;
				else throw error;
			}
		}
		assert.equal(running, false);
	} finally {
		await manager.shutdown();
		await rm(root, { recursive: true, force: true });
	}
});

test("failed state persistence never launches a command", async () => {
	const root = await mkdtemp(join(tmpdir(), "ct-background-persist-start-"));
	const directory = join(root, "private");
	const statePath = join(directory, "tasks.json");
	let launched = false;
	const operations: BashOperations = {
		exec: async () => {
			launched = true;
			return { exitCode: 0 };
		},
	};
	const manager = new BackgroundCommandManager({ storageDirectory: directory, cwd: root, operations });
	await manager.initialize();
	try {
		await rm(statePath);
		await mkdir(statePath);
		await assert.rejects(manager.start("must not run", root));
		assert.equal(launched, false);
		assert.deepEqual(manager.list(), []);
	} finally {
		await rm(statePath, { recursive: true, force: true });
		await manager.shutdown();
		await rm(root, { recursive: true, force: true });
	}
});

test("final persistence failure settles the task and releases the active slot", async () => {
	const root = await mkdtemp(join(tmpdir(), "ct-background-persist-end-"));
	const directory = join(root, "private");
	const statePath = join(directory, "tasks.json");
	let finish: ((result: { exitCode: number | null }) => void) | undefined;
	const operations: BashOperations = {
		exec: async () => await new Promise((resolve) => { finish = resolve; }),
	};
	const manager = new BackgroundCommandManager({ storageDirectory: directory, cwd: root, operations });
	await manager.initialize();
	try {
		const task = await manager.start("fixture", root);
		await rm(statePath);
		await mkdir(statePath);
		finish?.({ exitCode: 0 });
		await manager.waitForActive();
		assert.equal(manager.get(task.id).state, "failed");
		assert.match(manager.readOutput(task.id).text, /Unable to save background command state/);
	} finally {
		await rm(statePath, { recursive: true, force: true });
		await manager.shutdown();
		await rm(root, { recursive: true, force: true });
	}
});
