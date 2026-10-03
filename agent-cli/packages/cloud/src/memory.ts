import type { CloudThinkerClient } from "./client.ts";
import type { MemorySnapshot } from "./runtime.ts";
import { sleep } from "./tools/shared.ts";

export const MEMORY_MARKER = "<<<CT_MEMORY_INDEX>>>";
export const USERS_MARKER = "<<<CT_USER_NOTES>>>";
export const SANDBOX_HOME = "/home/user";
export const MEMORY_DIR = `${SANDBOX_HOME}/.memory`;
export const MAX_BLOCK_CHARS = 20_000;

export const MEMORY_SCRIPT = [
	`echo '${MEMORY_MARKER}'`,
	`cat ${MEMORY_DIR}/MEMORY.md 2>/dev/null`,
	`echo '${USERS_MARKER}'`,
	`cat ${MEMORY_DIR}/USERS.md 2>/dev/null`,
].join("; ");

const EXECUTION_TIMEOUT_SECONDS = 20;
const POLL_INTERVAL_MS = 5_000;
const MAX_WAIT_MS = 5 * 60 * 1000;

function clip(value: string): string {
	const trimmed = value.trim();
	return trimmed.length > MAX_BLOCK_CHARS
		? `${trimmed.slice(0, MAX_BLOCK_CHARS)}\n... (truncated)`
		: trimmed;
}

export function parseMemory(output: string): MemorySnapshot | undefined {
	const memoryAt = output.indexOf(MEMORY_MARKER);
	const usersAt = output.indexOf(USERS_MARKER, memoryAt + 1);
	if (memoryAt === -1 || usersAt === -1) return undefined;
	const memoryIndex = clip(output.slice(memoryAt + MEMORY_MARKER.length, usersAt));
	const userNotes = clip(output.slice(usersAt + USERS_MARKER.length));
	if (memoryIndex.length === 0 && userNotes.length === 0) return undefined;
	return { memoryIndex, userNotes };
}

export async function fetchMemory(
	client: CloudThinkerClient,
	conversationId: string,
	options: { intervalMs?: number; maxWaitMs?: number; now?: () => number } = {},
): Promise<MemorySnapshot | undefined> {
	const started = await client.execute({
		conversation_id: conversationId,
		connection_list: [],
		script: MEMORY_SCRIPT,
		timeout: EXECUTION_TIMEOUT_SECONDS,
		run_in_background: true,
	});
	if (started.status === "completed") return parseMemory(started.stdout);
	const now = options.now ?? Date.now;
	const deadline = now() + (options.maxWaitMs ?? MAX_WAIT_MS);
	let since = 0;
	let collected = "";
	for (;;) {
		const page = await client.readExecution(started.task_id, {
			conversation_id: conversationId,
			since,
		});
		collected += page.output;
		since = page.next_cursor;
		if (page.status !== "running") return parseMemory(collected);
		if (now() >= deadline) return undefined;
		await sleep(options.intervalMs ?? POLL_INTERVAL_MS);
	}
}
