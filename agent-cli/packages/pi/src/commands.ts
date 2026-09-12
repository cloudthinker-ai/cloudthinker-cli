import { getAgentDir } from "@earendil-works/pi-coding-agent";

import { CloudThinkerApiError } from "./client.ts";
import { PRODUCT_NAME } from "./header.ts";
import { type AutoMode, type CloudThinkerRuntime, describeError } from "./runtime.ts";
import { sessionPanelLines, sessionTotals } from "./usage.ts";
import { attributionLine, type HostVersions } from "./versions.ts";

export const SUBCOMMANDS = ["about", "session", "notify", "auto"] as const;

export const APPROVERS_NOTIFIED = "approvers notified";
export const NOTHING_WAITING = "nothing is waiting for approval";
export const AUTO_MODE_EDITOR_HINT =
	"a workspace settings editor can change it in Agent settings";

export function approvalModeSentence(enabled: boolean): string {
	return enabled
		? "Approval mode: Auto — a cloud write runs when the workspace rule allows it"
		: "Approval mode: Manual — every cloud write waits for a human";
}

export function autoModeLines(mode: AutoMode): string[] {
	return [
		approvalModeSentence(mode.enabled),
		mode.canEdit ? `change it with /${PRODUCT_NAME} auto on|off` : AUTO_MODE_EDITOR_HINT,
	];
}

export interface Notifier {
	notify(message: string, type?: "info" | "warning" | "error"): void;
}

export async function notifyApprovers(runtime: CloudThinkerRuntime, ui: Notifier): Promise<void> {
	const thread = runtime.askThread;
	if (!thread) {
		ui.notify(NOTHING_WAITING, "info");
		return;
	}
	try {
		await runtime.client.resendInterruptNotification(thread.conversation_id);
		ui.notify(APPROVERS_NOTIFIED, "info");
	} catch (error) {
		if (error instanceof CloudThinkerApiError && error.status === 404) {
			ui.notify(NOTHING_WAITING, "info");
			return;
		}
		ui.notify(describeError(error), "error");
	}
}

export async function autoCommand(
	runtime: CloudThinkerRuntime,
	ui: Notifier,
	argument: string,
): Promise<void> {
	const mode = runtime.autoMode;
	const session = runtime.session;
	if (!mode || !session) {
		ui.notify("This session is not linked to CloudThinker.", "warning");
		return;
	}
	if (argument === "") {
		ui.notify(autoModeLines(mode).join("\n"), "info");
		return;
	}
	if (argument !== "on" && argument !== "off") {
		ui.notify(`Usage: /${PRODUCT_NAME} auto [on|off]`, "warning");
		return;
	}
	try {
		const status = await runtime.client.setWorkspaceAutoMode(
			session.workspace_id,
			argument === "on",
		);
		runtime.setAutoMode({ ...mode, enabled: status.enabled });
		ui.notify(approvalModeSentence(status.enabled), "info");
	} catch (error) {
		if (error instanceof CloudThinkerApiError && error.status === 403) {
			ui.notify(AUTO_MODE_EDITOR_HINT, "warning");
			return;
		}
		ui.notify(describeError(error), "error");
	}
}

export function aboutLines(
	versions: HostVersions,
	agentDir: string,
	webUrl: string | undefined,
): string[] {
	return [
		`${PRODUCT_NAME} v${versions.host}`,
		...(versions.buildId ? [`Build: ${versions.buildId}`] : []),
		`pi v${versions.pi}`,
		attributionLine(versions),
		`Config: ${agentDir}`,
		webUrl ? `Session: ${webUrl}` : "Session: not linked",
	];
}

function opener(): { command: string; args: string[] } | undefined {
	if (process.platform === "darwin") return { command: "open", args: [] };
	if (process.platform === "win32") return { command: "cmd", args: ["/c", "start", ""] };
	if (process.platform === "linux") return { command: "xdg-open", args: [] };
	return undefined;
}

export function registerCommands(runtime: CloudThinkerRuntime): void {
	runtime.pi.registerCommand("open", {
		description: "Open this session's CloudThinker conversation in the browser",
		handler: async (_args, ctx) => {
			const session = runtime.session;
			if (!session) {
				ctx.ui.notify("This session is not linked to CloudThinker.", "warning");
				return;
			}
			ctx.ui.notify(session.web_url, "info");
			const launcher = opener();
			if (!launcher) return;
			try {
				await runtime.pi.exec(launcher.command, [...launcher.args, session.web_url], {
					timeout: 5_000,
				});
			} catch {
				return;
			}
		},
	});

	runtime.pi.registerCommand("cloud", {
		description: "Show Cloud status or turn remote commands and Anna delegation on/off",
		getArgumentCompletions: (prefix) =>
			["on", "off"].filter((name) => name.startsWith(prefix)).map((name) => ({ value: name, label: name })),
		handler: async (args, ctx) => {
			const argument = args.trim();
			if (argument && argument !== "on" && argument !== "off") {
				ctx.ui.notify("Usage: /cloud [on|off]", "warning");
				return;
			}
			if (argument) {
				if (!ctx.isIdle()) {
					ctx.ui.notify("Wait for this turn to finish or stop it before changing Cloud.", "warning");
					return;
				}
				runtime.bind(ctx);
				runtime.setCloudEnabled(argument === "on");
			}

			const lines: string[] = [`Cloud: ${runtime.cloudEnabled ? "On" : "Off"}`, "Change with /cloud on|off. Off disables remote commands and Anna delegation; existing remote work continues."];
			lines.push(
				runtime.identity
					? `Workspace: ${runtime.identity.workspace_name} (${runtime.identity.user_email})`
					: "Workspace: unknown (identity call failed)",
			);
			lines.push(
				`Connections: ${runtime.connectedPrefixes.join(", ") || "none connected"}`,
			);
			lines.push(
				runtime.session
					? `Mirror: ${runtime.session.web_url}`
					: "Mirror: not linked, cloud tools are unavailable",
			);
			if (runtime.askThread) lines.push(`Anna thread: ${runtime.askThread.web_url}`);
			ctx.ui.notify(lines.join("\n"), "info");
		},
	});

	runtime.pi.registerCommand(PRODUCT_NAME, {
		description: `CloudThinker agent subcommands: ${SUBCOMMANDS.join(", ")}`,
		getArgumentCompletions: (prefix) =>
			SUBCOMMANDS.filter((name) => name.startsWith(prefix)).map((name) => ({
				value: name,
				label: name,
			})),
		handler: async (args, ctx) => {
			const [subcommand = "", ...rest] = args.trim().split(/\s+/);
			if (subcommand === "notify") {
				await notifyApprovers(runtime, ctx.ui);
				return;
			}
			if (subcommand === "auto") {
				await autoCommand(runtime, ctx.ui, rest.join(" "));
				return;
			}
			if (subcommand === "about") {
				ctx.ui.notify(
					aboutLines(runtime.versions, getAgentDir(), runtime.session?.web_url).join(
						"\n",
					),
					"info",
				);
				return;
			}
			if (subcommand === "session") {
				ctx.ui.notify(
					sessionPanelLines(
						{
							file: ctx.sessionManager.getSessionFile(),
							id: ctx.sessionManager.getSessionId(),
							name: ctx.sessionManager.getSessionName(),
							webUrl: runtime.session?.web_url,
						},
						sessionTotals(ctx.sessionManager.getEntries()),
						runtime.credits,
					).join("\n"),
					"info",
				);
				return;
			}
			ctx.ui.notify(`Usage: /${PRODUCT_NAME} ${SUBCOMMANDS.join("|")}`, "warning");
		},
	});
}
