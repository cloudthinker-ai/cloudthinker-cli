import { appendFile, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { dirname } from "node:path";

import type { MirrorEntryInput } from "./client.ts";
import { outboxPath, rejectedPath } from "./paths.ts";

export class MirrorOutbox {
	private readonly agentDir: string | undefined;
	private operations: Promise<void> = Promise.resolve();

	constructor(agentDir: string | undefined) {
		this.agentDir = agentDir;
	}

	async withOperation<T>(operation: () => Promise<T>): Promise<T> {
		const previous = this.operations;
		let release!: () => void;
		this.operations = new Promise<void>((resolve) => {
			release = resolve;
		});
		await previous;
		try {
			return await operation();
		} finally {
			release();
		}
	}

	async read(conversationId: string): Promise<MirrorEntryInput[]> {
		let raw: string;
		try {
			raw = await readFile(outboxPath(conversationId, this.agentDir), "utf8");
		} catch {
			return [];
		}
		const entries: MirrorEntryInput[] = [];
		for (const line of raw.split("\n")) {
			if (line.trim().length === 0) continue;
			try {
				entries.push(JSON.parse(line) as MirrorEntryInput);
			} catch {}
		}
		return entries;
	}

	async append(conversationId: string, entries: MirrorEntryInput[]): Promise<void> {
		if (entries.length === 0) return;
		const path = outboxPath(conversationId, this.agentDir);
		await mkdir(dirname(path), { recursive: true });
		const lines = entries.map((entry) => JSON.stringify(entry)).join("\n");
		await appendFile(path, `\n${lines}\n`, "utf8");
	}

	async remove(conversationId: string): Promise<void> {
		await rm(outboxPath(conversationId, this.agentDir), { force: true });
	}

	async readRejected(conversationId: string): Promise<Set<string>> {
		try {
			const parsed: unknown = JSON.parse(
				await readFile(rejectedPath(conversationId, this.agentDir), "utf8"),
			);
			if (Array.isArray(parsed)) {
				return new Set(parsed.filter((item): item is string => typeof item === "string"));
			}
		} catch {}
		return new Set();
	}

	async writeRejected(conversationId: string, rejected: Set<string>): Promise<void> {
		const path = rejectedPath(conversationId, this.agentDir);
		await mkdir(dirname(path), { recursive: true });
		await writeFile(path, JSON.stringify([...rejected]), "utf8");
	}
}
