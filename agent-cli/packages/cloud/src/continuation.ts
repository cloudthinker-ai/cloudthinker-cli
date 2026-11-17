import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

import { PROVIDER_ID } from "./provider.ts";

export const CONTINUE_ENTRY = "cloudthinker-continue";
export const MAX_CONTINUATIONS = 2;
export const CONTINUE_PROMPT = "Your last reply was cut off at the output limit before the task was done. Continue exactly where it stopped: do not repeat what you already wrote, and keep each reply short enough to finish. When the task is done, give the final answer; if you are blocked, name the concrete blocker.";
export const LIMIT_NOTICE = "The model stopped at its output limit again, so the run ended unfinished. Send \"continue\" to resume it.";

export function registerContinuation(pi: ExtensionAPI): void {
	let continued = 0;
	pi.on("before_agent_start", () => {
		continued = 0;
	});
	pi.on("turn_end", (event) => {
		if (event.message.role === "assistant" && event.message.stopReason !== "length") continued = 0;
	});
	pi.on("message_end", (event) => {
		const message = event.message;
		if (message.role !== "assistant" || message.provider !== PROVIDER_ID || message.stopReason !== "length") return;
		if (continued < MAX_CONTINUATIONS) return;
		return { message: { ...message, stopReason: "error", errorMessage: LIMIT_NOTICE } };
	});
	pi.on("agent_before_settle", (event) => {
		if (event.outcome !== "completed" || event.continue || event.context.pendingMessages.length > 0) return;
		const last = event.context.contextMessages.at(-1);
		if (last?.role !== "assistant" || last.provider !== PROVIDER_ID || last.stopReason !== "length") return;
		if (continued >= MAX_CONTINUATIONS) return;
		continued += 1;
		return {
			entries: [...event.entries, { type: "custom_message", customType: CONTINUE_ENTRY, content: CONTINUE_PROMPT, display: false }],
			continue: true,
		};
	});
}
