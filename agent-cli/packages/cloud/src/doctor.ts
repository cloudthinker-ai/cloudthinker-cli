import type { ExtensionAPI, ExtensionCommandContext } from "@earendil-works/pi-coding-agent";

import { sanitizeTerminalText } from "./awareness.ts";
import { tokenBinary } from "./client.ts";

export const DOCTOR_COMMAND = "doctor";
export const DOCTOR_SKILL = "doctor";
const DOCTOR_TIMEOUT_MS = 120_000;
const ALL_PASSED = /^✓ doctor · .+ · all checks passed$/;

export function doctorText(stdout: string): string {
	return stdout.split("\n").map((line) => sanitizeTerminalText(line).trimEnd()).join("\n").replace(/^\n+|\n+$/g, "");
}

export function allPassed(text: string, code: number): boolean {
	return code === 0 && !text.includes("\n") && ALL_PASSED.test(text.trim());
}

export function doctorPrompt(text: string): string {
	return `/skill:${DOCTOR_SKILL} /doctor ran \`cloudthinker doctor\` and printed:\n\n\`\`\`text\n${text}\n\`\`\``;
}

export function launcherFailure(binary: string, code: number, stderr: string): string {
	const reason = sanitizeTerminalText(stderr.split("\n").find((line) => line.trim()) ?? "").trim().slice(0, 200);
	return [
		`/doctor could not run \`${binary} doctor\` (exit ${code})${reason ? `: ${reason}` : "."}`,
		"Check that cloudthinker is installed and up to date with `cloudthinker update`, then run `cloudthinker doctor` in another terminal.",
	].join("\n");
}

export function registerDoctor(pi: ExtensionAPI): void {
	pi.registerCommand(DOCTOR_COMMAND, {
		description: "Check this CLI's install, sign-in, network and Connections, and explain any problem",
		handler: async (_args: string, ctx: ExtensionCommandContext) => {
			const binary = tokenBinary();
			ctx.ui.setStatus(DOCTOR_COMMAND, "running doctor…");
			const result = await pi.exec(binary, ["doctor"], { timeout: DOCTOR_TIMEOUT_MS }).finally(() => ctx.ui.setStatus(DOCTOR_COMMAND, undefined));
			const text = doctorText(result.stdout);
			if (!text) {
				ctx.ui.notify(launcherFailure(binary, result.code, result.stderr), "error");
				return;
			}
			ctx.ui.notify(text, "info");
			if (allPassed(text, result.code)) return;
			pi.sendUserMessage(doctorPrompt(text), ctx.isIdle() ? { expandPromptTemplates: true } : { expandPromptTemplates: true, deliverAs: "followUp" });
		},
	});
}
