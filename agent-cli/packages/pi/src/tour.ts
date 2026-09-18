import type { ExtensionCommandContext } from "@earendil-works/pi-coding-agent";

import { CLOUD_TAG, LOCAL_TAG, sanitizeTerminalText } from "./awareness.ts";
import { type CloudThinkerRuntime, describeError } from "./runtime.ts";
import { renderExecution } from "./tools/ct-sandbox-read.ts";

export const TOUR_OFFER = "New here? /tour shows which machine runs what";
export const TOUR_GIT = "git status --short";
export const TOUR_LOCAL_UNAVAILABLE = "Nothing local could be read here.";
export const TOUR_CLOUD_SCRIPT = "uname -a";
export const TOUR_LOCAL_OUTPUT_LIMIT = 4_000;
const TOUR_TRUNCATED = "(truncated)";

export interface TourHalf {
	label: string;
	body: string;
}

function boundedLocalOutput(text: string): string {
	const trimmed = sanitizeTerminalText(text).trimEnd();
	if (trimmed.length <= TOUR_LOCAL_OUTPUT_LIMIT) return trimmed;
	return `${trimmed.slice(0, TOUR_LOCAL_OUTPUT_LIMIT)}\n${TOUR_TRUNCATED}`;
}

export async function tourLocal(runtime: CloudThinkerRuntime): Promise<TourHalf> {
	const git = await runtime.pi.exec("git", ["status", "--short"], { timeout: 5_000 }).catch(() => undefined);
	if (git && git.code === 0) return { label: TOUR_GIT, body: boundedLocalOutput(git.stdout) || "(nothing to report)" };
	return { label: TOUR_GIT, body: TOUR_LOCAL_UNAVAILABLE };
}

export async function tourCloud(
	runtime: CloudThinkerRuntime,
	ctx: ExtensionCommandContext,
): Promise<TourHalf> {
	if (!runtime.cloudEnabled) {
		return { label: "off for this session", body: "Cloud is off, so nothing runs there. /cloud on enables the sandbox and its Connections." };
	}
	const session = runtime.session;
	if (!session) {
		return { label: "not linked", body: "This session is not linked to CloudThinker, so the sandbox is unavailable." };
	}
	const prefixes = runtime.connectedPrefixes;
	if (prefixes.length === 0) {
		return { label: "no connection", body: "No Connection is connected in this workspace, so there is nothing to read there yet." };
	}
	let prefix = prefixes[0]!;
	if (prefixes.length > 1) {
		const chosen = await ctx.ui.select("Which Connection should the tour read?", prefixes);
		if (!chosen) return { label: "nothing chosen", body: "No Connection was chosen, so the tour read only your machine." };
		prefix = chosen;
	}
	try {
		const result = await runtime.client.execute({
			conversation_id: session.conversation_id,
			connection_list: [prefix],
			script: TOUR_CLOUD_SCRIPT,
			timeout: 30,
			run_in_background: false,
		});
		return { label: `${sanitizeTerminalText(prefix)} - ${TOUR_CLOUD_SCRIPT}`, body: sanitizeTerminalText(renderExecution(result)) };
	} catch (error) {
		return { label: prefix, body: describeError(error) };
	}
}

export function tourLines(local: TourHalf, cloud: TourHalf): string[] {
	return [
		`${LOCAL_TAG} this machine - ${local.label}`,
		local.body,
		"",
		`${CLOUD_TAG} CloudThinker Sandbox - ${cloud.label}`,
		cloud.body,
	];
}

export async function runTour(runtime: CloudThinkerRuntime, ctx: ExtensionCommandContext): Promise<void> {
	if (!ctx.isIdle()) {
		ctx.ui.notify("Wait for this turn to finish or stop it before running the tour.", "warning");
		return;
	}
	const local = await tourLocal(runtime);
	const cloud = await tourCloud(runtime, ctx);
	ctx.ui.notify(tourLines(local, cloud).join("\n"), "info");
}
