import { spawn } from "node:child_process";
import { readFile } from "node:fs/promises";

import { copyToClipboard, getAgentDir } from "@earendil-works/pi-coding-agent";
import type { ExtensionCommandContext } from "@earendil-works/pi-coding-agent";

import { CloudThinkerApiError, tokenBinary } from "./client.ts";
import { PRODUCT_NAME } from "./header.ts";
import { sanitizeTerminalText, setMachineState } from "./awareness.ts";
import { type AutoMode, type CloudThinkerRuntime, describeError } from "./runtime.ts";
import { newSessionEvent, refreshIdentity, restoreModels, retryLink, startSession } from "./session.ts";
import { sessionPanelLines, sessionTotals } from "./usage.ts";
import { workspaceCommand } from "./workspace.ts";
import { attributionLine, type HostVersions } from "./versions.ts";

export const SUBCOMMANDS = ["about", "session", "notify", "auto", "workspace", "share", "login", "logout", "changelog", "bug"] as const;

export const BUG_URL = "https://github.com/cloudthinker-ai/cloudthinker-cli/issues/new";
export const CHANGELOG_URL = "https://docs.cloudthinker.io/changelog";
export const CHANGELOG_RELEASES = 3;
export const LOGIN_TIMEOUT_MS = 10 * 60_000;

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

export interface WhereState {
	cwd: string;
	workspaceName?: string;
	connectedPrefixes: string[];
	cloudEnabled: boolean;
	linked: boolean;
}

export const WHERE_SIDE_LINE =
	"Tools marked cloud run in the cloud; everything else runs on your machine. Credentials never leave the cloud.";

export function whereLines(state: WhereState): string[] {
	const local = `local · ${sanitizeTerminalText(state.cwd)} — your files, your shell, your git state`;
	const workspaceName = state.workspaceName === undefined ? "workspace" : sanitizeTerminalText(state.workspaceName);
	let cloud: string;
	if (!state.linked) {
		cloud = "cloud · not linked — workspace tools and delegation are unavailable";
	} else if (!state.cloudEnabled) {
		cloud = `cloud · ${workspaceName} — workspace tools are off; /cloud on enables them`;
	} else {
		const connections =
			state.connectedPrefixes.length > 0
				? state.connectedPrefixes.map(sanitizeTerminalText).join(", ")
				: "no connections yet";
		cloud = `cloud · ${workspaceName} — workspace machine, ${connections}`;
	}
	return ["I work in two places:", local, cloud, WHERE_SIDE_LINE];
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
		...(versions.buildId ? [`Build: ${sanitizeTerminalText(versions.buildId)}`] : []),
		`pi v${versions.pi}`,
		attributionLine(versions),
		`Config: ${sanitizeTerminalText(agentDir)}`,
		webUrl ? `Session: ${sanitizeTerminalText(webUrl)}` : "Session: not linked",
	];
}

export function bugReportUrl(versions: HostVersions, title: string): string {
	const body = [
		"**What happened**",
		"",
		"",
		"**What you expected**",
		"",
		"",
		"**Steps to reproduce**",
		"",
		"",
		"---",
		`cloudthinker agent ${versions.host}${versions.buildId ? ` (${versions.buildId})` : ""} · pi ${versions.pi} · ${process.platform} ${process.arch}`,
	].join("\n");
	return `${BUG_URL}?${new URLSearchParams({ title, body }).toString()}`;
}

export function latestReleases(changelog: string, count: number = CHANGELOG_RELEASES): string {
	const sections = changelog.split(/^(?=## )/m).filter((section) => section.startsWith("## "));
	return sections.slice(0, count).map((section) => section.trimEnd()).join("\n\n");
}

export function shareLines(webUrl: string, workspaceName: string | undefined, copied: boolean): string[] {
	const audience = workspaceName ? `members of ${sanitizeTerminalText(workspaceName)}` : "members of this workspace";
	return [
		`${copied ? "Copied this conversation's link" : "This conversation's link"} (only ${audience} can open it):`,
		sanitizeTerminalText(webUrl),
	];
}

export function runCli(
	args: string[],
	onLine: (line: string) => void,
	signal?: AbortSignal,
	env: NodeJS.ProcessEnv = process.env,
): Promise<number> {
	const { CLOUDTHINKER_WORKSPACE: _workspace, ...rest } = env;
	return new Promise((resolve) => {
		const child = spawn(tokenBinary(env), args, { env: rest, stdio: ["ignore", "pipe", "pipe"], timeout: LOGIN_TIMEOUT_MS, signal });
		const emit = (line: string) => {
			if (line.trim()) onLine(sanitizeTerminalText(line));
		};
		const lines = (stream: NodeJS.ReadableStream) => {
			let pending = "";
			stream.setEncoding("utf8");
			stream.on("data", (chunk: string) => {
				const parts = (pending + chunk).split(/\r?\n/);
				pending = parts.pop() ?? "";
				parts.forEach(emit);
			});
			stream.on("end", () => emit(pending));
		};
		lines(child.stdout);
		lines(child.stderr);
		child.on("error", (error) => {
			if (!signal?.aborted) onLine(`Could not run ${tokenBinary(env)}: ${error.message}`);
			resolve(1);
		});
		child.on("close", (code) => resolve(code ?? 1));
	});
}

function opener(): { command: string; args: string[] } | undefined {
	if (process.platform === "darwin") return { command: "open", args: [] };
	if (process.platform === "win32") return { command: "cmd", args: ["/c", "start", ""] };
	if (process.platform === "linux") return { command: "xdg-open", args: [] };
	return undefined;
}

async function openUrl(runtime: CloudThinkerRuntime, url: string): Promise<void> {
	const launcher = opener();
	if (!launcher) return;
	try {
		await runtime.pi.exec(launcher.command, [...launcher.args, url], { timeout: 5_000 });
	} catch {
		return;
	}
}

async function shareCommand(runtime: CloudThinkerRuntime, ctx: ExtensionCommandContext): Promise<void> {
	const session = runtime.session;
	if (!session) {
		ctx.ui.notify("This session is not linked to CloudThinker, so it has no link yet; /cloud retry links it.", "warning");
		return;
	}
	let copied = false;
	try {
		await copyToClipboard(session.web_url);
		copied = true;
	} catch {
		copied = false;
	}
	ctx.ui.notify(shareLines(session.web_url, runtime.identity?.workspace_name, copied).join("\n"), "info");
}

async function loginCommand(runtime: CloudThinkerRuntime, ctx: ExtensionCommandContext): Promise<void> {
	if (!ctx.isIdle()) {
		ctx.ui.notify("Wait for this turn to finish or stop it before signing in.", "warning");
		return;
	}
	const lines = ["Signing in to CloudThinker… (Esc cancels)"];
	ctx.ui.notify(lines[0]!, "info");
	const cancel = new AbortController();
	const stopListening = ctx.ui.onTerminalInput((data) => {
		if (data !== "\x1b") return undefined;
		cancel.abort();
		return { consume: true };
	});
	let code: number;
	try {
		code = await runCli(["login"], (line) => {
			lines.push(line);
			ctx.ui.notify(lines.join("\n"), "info");
		}, cancel.signal);
	} finally {
		stopListening();
	}
	if (cancel.signal.aborted) {
		ctx.ui.notify("Sign-in cancelled; run /login to try again.", "info");
		return;
	}
	if (code !== 0) {
		ctx.ui.notify("Sign-in did not finish; run /login to try again.", "warning");
		return;
	}
	runtime.bind(ctx);
	if (!runtime.cloudEnabled) {
		ctx.ui.notify("Signed in. Cloud is off; /cloud on links this session.", "info");
		return;
	}
	if (runtime.session) {
		await refreshIdentity(runtime);
		await restoreModels(runtime, ctx);
	} else {
		await retryLink(runtime, ctx);
	}
	ctx.ui.notify(runtime.session ? "Signed in and linked to CloudThinker." : "Signed in; the cloud link is still retrying.", "info");
}

async function logoutCommand(runtime: CloudThinkerRuntime, ctx: ExtensionCommandContext): Promise<void> {
	const code = await runCli(["logout"], (line) => ctx.ui.notify(line, "info"));
	if (code !== 0) {
		ctx.ui.notify("Sign-out failed; run cloudthinker logout in a shell.", "error");
		return;
	}
	runtime.client.invalidateTokens();
	runtime.reset();
	ctx.ui.notify("Signed out. Cloud tools stop working until you /login again.", "info");
}

async function changelogCommand(ctx: ExtensionCommandContext, changelogPath: string | undefined): Promise<void> {
	let releases = "";
	try {
		releases = changelogPath ? latestReleases(await readFile(changelogPath, "utf8")) : "";
	} catch {
		releases = "";
	}
	ctx.ui.notify(releases ? `${releases}\n\nAll releases: ${CHANGELOG_URL}` : `What's new: ${CHANGELOG_URL}`, "info");
}

async function bugCommand(runtime: CloudThinkerRuntime, ctx: ExtensionCommandContext, title: string): Promise<void> {
	const url = bugReportUrl(runtime.versions, title);
	ctx.ui.notify(`Report a bug (your conversation is not attached): ${url}`, "info");
	await openUrl(runtime, url);
}

export interface CommandOptions {
	changelogPath?: string;
}

export function registerCommands(runtime: CloudThinkerRuntime, options: CommandOptions = {}): void {
	runtime.pi.registerCommand("open", {
		description: "Open this session's CloudThinker conversation in the browser",
		handler: async (_args, ctx) => {
			const session = runtime.session;
			if (!session) {
				ctx.ui.notify("This session is not linked to CloudThinker.", "warning");
				return;
			}
			ctx.ui.notify(session.web_url, "info");
			await openUrl(runtime, session.web_url);
		},
	});

	runtime.pi.registerCommand("cloud", {
		description: "Show Cloud status, turn workspace tools on/off, or retry the cloud link",
		getArgumentCompletions: (prefix) =>
			["on", "off", "retry"].filter((name) => name.startsWith(prefix)).map((name) => ({ value: name, label: name })),
		handler: async (args, ctx) => {
			const argument = args.trim();
			if (argument && argument !== "on" && argument !== "off" && argument !== "retry") {
				ctx.ui.notify("Usage: /cloud [on|off|retry]", "warning");
				return;
			}
			if (argument === "retry") {
				if (!runtime.cloudEnabled) {
					ctx.ui.notify("Cloud is off; /cloud on links it.", "info");
					return;
				}
				if (runtime.session) {
					ctx.ui.notify(await restoreModels(runtime, ctx) ? "Agent modes are back; /model picks one." : "Cloud is already linked.", "info");
					return;
				}
				await retryLink(runtime, ctx);
				return;
			} else if (argument) {
				if (!ctx.isIdle()) {
					ctx.ui.notify("Wait for this turn to finish or stop it before changing Cloud.", "warning");
					return;
				}
				runtime.bind(ctx);
				runtime.setCloudEnabled(argument === "on");
				if (argument === "on") {
					if (!runtime.session) {
						await startSession(runtime, runtime.startEvent ?? newSessionEvent(), ctx, runtime.sourceConversationId);
						if (runtime.root) {
							setMachineState({
								linked: runtime.session !== undefined,
								workspaceName: runtime.identity?.workspace_name,
								connectionCount: runtime.connectedPrefixes.length,
							});
						}
					} else {
						await refreshIdentity(runtime);
					}
					await runtime.afterLink?.(ctx);
				}
			}

			const lines: string[] = [`Cloud: ${runtime.cloudEnabled ? "On" : "Off"}`, "Change with /cloud on|off. Off disables workspace tools and delegation; existing remote work continues."];
			lines.push(
				runtime.identity
					? `Workspace: ${sanitizeTerminalText(runtime.identity.workspace_name)} (${sanitizeTerminalText(runtime.identity.user_email)})`
					: "Workspace: unknown (identity call failed)",
			);
			lines.push(
				`Connections: ${runtime.connectedPrefixes.map(sanitizeTerminalText).join(", ") || "none connected"}`,
			);
			lines.push(
				runtime.session
					? `Mirror: ${sanitizeTerminalText(runtime.session.web_url)}`
					: "Mirror: not linked, cloud tools are unavailable",
			);
			if (runtime.askThread) lines.push(`Delegation thread: ${sanitizeTerminalText(runtime.askThread.web_url)}`);
			ctx.ui.notify(lines.join("\n"), "info");
		},
	});

	runtime.pi.registerCommand("where", {
		description: "Show the two places this agent works: your machine and the cloud workspace",
		handler: async (_args, ctx) => {
			ctx.ui.notify(
				whereLines({
					cwd: process.cwd(),
					workspaceName: runtime.identity?.workspace_name,
					connectedPrefixes: runtime.connectedPrefixes,
					cloudEnabled: runtime.cloudEnabled,
					linked: runtime.session !== undefined,
				}).join("\n"),
				"info",
			);
		},
	});

	runtime.pi.registerCommand(PRODUCT_NAME, {
		description: `CloudThinker Agent subcommands: ${SUBCOMMANDS.join(", ")}`,
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
			if (subcommand === "workspace") {
				await workspaceCommand(runtime, ctx, rest.join(" "));
				return;
			}
			if (subcommand === "share") {
				await shareCommand(runtime, ctx);
				return;
			}
			if (subcommand === "login") {
				await loginCommand(runtime, ctx);
				return;
			}
			if (subcommand === "logout") {
				await logoutCommand(runtime, ctx);
				return;
			}
			if (subcommand === "changelog") {
				await changelogCommand(ctx, options.changelogPath);
				return;
			}
			if (subcommand === "bug") {
				await bugCommand(runtime, ctx, rest.join(" "));
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
