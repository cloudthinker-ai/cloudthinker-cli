import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

import { writeRawStdout } from "../node_modules/@earendil-works/pi-coding-agent/dist/core/output-guard.js";
import { takeFlagValues } from "./argv.ts";

export const MAX_TURNS_FLAG = "--max-turns";

export function maxTurnsLine(limit: number): string {
	return `Stopped at --max-turns ${limit}: the agent had not finished.`;
}

export function extractMaxTurns(argv: readonly string[]): { limit: number | undefined; rest: string[] } {
	const { values, rest } = takeFlagValues(argv, MAX_TURNS_FLAG);
	const last = values.at(-1);
	return { limit: last === undefined ? undefined : Number(last), rest };
}

export function registerMaxTurns(pi: Pick<ExtensionAPI, "on">, limit: number | undefined): void {
	if (limit === undefined) return;
	let turns = 0;
	let stopped = false;
	pi.on("turn_end", (event, ctx) => {
		if (ctx.mode !== "print" && ctx.mode !== "json") return;
		turns += 1;
		if (stopped || turns < limit || event.toolResults.length === 0) return;
		stopped = true;
		process.exitCode = 1;
		if (ctx.mode === "json") {
			writeRawStdout(`${JSON.stringify({ type: "max_turns", maxTurns: limit })}\n`);
			process.stderr.write(`${maxTurnsLine(limit)}\n`);
		}
		ctx.abort();
	});
	pi.on("message_end", (event) => {
		if (!stopped || event.message.role !== "assistant" || event.message.stopReason !== "aborted") return undefined;
		return { message: { ...event.message, errorMessage: maxTurnsLine(limit) } };
	});
}
