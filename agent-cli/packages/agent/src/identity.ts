import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

export const PI_PREAMBLE = "You are an expert coding assistant operating inside pi, a coding agent harness. You help users by reading files, executing commands, editing code, and writing new files.";
export const CLOUDTHINKER_PREAMBLE = "You are CloudThinker Agent, running in the user's terminal. You help users understand, operate, secure, and optimize their code and cloud infrastructure by reading files, executing commands, editing code, and writing new files.";

export function withCloudThinkerIdentity(systemPrompt: string): string {
	return systemPrompt.startsWith(PI_PREAMBLE) ? `${CLOUDTHINKER_PREAMBLE}${systemPrompt.slice(PI_PREAMBLE.length)}` : systemPrompt;
}

export function registerIdentity(pi: ExtensionAPI): void {
	pi.on("before_agent_start", (event) => ({ systemPrompt: withCloudThinkerIdentity(event.systemPrompt) }));
}
