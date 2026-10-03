import type { ExtensionCommandContext } from "@earendil-works/pi-coding-agent";

import { sanitizeTerminalText } from "./awareness.ts";
import { type AgentCliWorkspace, type CommandRunner, resolveBaseUrl, runCommand, tokenBinary } from "./client.ts";
import { registerProvider } from "./provider.ts";
import { type CloudThinkerRuntime, describeError } from "./runtime.ts";

export const WORKSPACE_ENV_VAR = "CLOUDTHINKER_WORKSPACE";
export const TOKEN_PINS_WORKSPACE =
	"CLOUDTHINKER_TOKEN pins this session to one workspace. Unset it to switch workspaces.";
export const ONLY_ONE_WORKSPACE =
	"You have only one workspace, or only one is logged in. Run `cloudthinker login` to add another, then use /cloudthinker workspace.";

export interface StoredWorkspace {
	id: string;
	name: string;
	active: boolean;
}

export interface WorkspaceDeps {
	run: CommandRunner;
	env: NodeJS.ProcessEnv;
}

const defaultDeps = (): WorkspaceDeps => ({ run: runCommand, env: process.env });

function cliArgs(env: NodeJS.ProcessEnv, ...rest: string[]): string[] {
	return ["--url", resolveBaseUrl(env), ...rest];
}

export function parseStoredWorkspaces(stdout: string): StoredWorkspace[] {
	const parsed = JSON.parse(stdout) as { workspaces?: unknown };
	if (!Array.isArray(parsed.workspaces)) return [];
	return parsed.workspaces.flatMap((entry): StoredWorkspace[] => {
		const item = entry as Record<string, unknown>;
		if (typeof item.workspace_id !== "string" || typeof item.workspace_name !== "string") return [];
		return [{ id: item.workspace_id, name: item.workspace_name, active: item.active === true }];
	});
}

export async function listStoredWorkspaces(deps: WorkspaceDeps): Promise<StoredWorkspace[]> {
	const result = await deps.run(tokenBinary(deps.env), cliArgs(deps.env, "auth", "status", "--json"));
	if (result.status !== 0) throw new Error("`cloudthinker auth status` failed. Run `cloudthinker login`.");
	return parseStoredWorkspaces(result.stdout);
}

export interface WorkspaceEntry {
	id: string;
	name: string;
	organizationName: string | null;
	loggedIn: boolean;
	current: boolean;
}

export const PERSONAL_GROUP = "Personal";
export const LOGGED_IN_GROUP = "Logged in";

export function buildEntries(
	stored: StoredWorkspace[],
	remote: AgentCliWorkspace[] | undefined,
	currentId: string | undefined,
): WorkspaceEntry[] {
	const loggedIn = new Set(stored.map((workspace) => workspace.id));
	if (!remote) {
		return stored.map((workspace) => ({
			id: workspace.id,
			name: workspace.name,
			organizationName: null,
			loggedIn: true,
			current: workspace.id === currentId,
		}));
	}
	return remote.map((workspace) => ({
		id: workspace.workspace_id,
		name: workspace.workspace_name,
		organizationName: workspace.organization_name,
		loggedIn: loggedIn.has(workspace.workspace_id),
		current: workspace.workspace_id === currentId,
	}));
}

export interface WorkspaceMenu {
	options: string[];
	byLabel: Map<string, WorkspaceEntry>;
}

export function workspaceMenu(entries: WorkspaceEntry[], grouped: boolean): WorkspaceMenu {
	const groupName = (entry: WorkspaceEntry): string =>
		entry.organizationName ? sanitizeTerminalText(entry.organizationName) : grouped ? PERSONAL_GROUP : LOGGED_IN_GROUP;
	const groups = new Map<string, WorkspaceEntry[]>();
	for (const entry of entries) {
		const key = groupName(entry);
		groups.set(key, [...(groups.get(key) ?? []), entry]);
	}
	const ordered = [...groups.entries()].sort(([left], [right]) => {
		if (left === PERSONAL_GROUP) return 1;
		if (right === PERSONAL_GROUP) return -1;
		return left.localeCompare(right);
	});
	const options: string[] = [];
	const byLabel = new Map<string, WorkspaceEntry>();
	for (const [group, members] of ordered) {
		options.push(group);
		const names = new Map<string, number>();
		for (const member of members) names.set(member.name, (names.get(member.name) ?? 0) + 1);
		for (const member of members.sort((left, right) => left.name.localeCompare(right.name))) {
			const duplicate = (names.get(member.name) ?? 0) > 1;
			const label = `  ${sanitizeTerminalText(member.name)}${duplicate ? ` · ${member.id.slice(0, 8)}` : ""}${member.current ? "  (current)" : ""}${member.loggedIn ? "" : "  · login needed"}`;
			options.push(label);
			byLabel.set(label, member);
		}
	}
	return { options, byLabel };
}

export function matchWorkspace(entries: WorkspaceEntry[], query: string): WorkspaceEntry | string {
	const byId = entries.find((entry) => entry.id === query);
	if (byId) return byId;
	const byName = entries.filter((entry) => entry.name.toLowerCase() === query.toLowerCase());
	if (byName.length === 1) return byName[0] as WorkspaceEntry;
	if (byName.length > 1) return `More than one workspace is named "${sanitizeTerminalText(query)}". Pick it from the list or pass its id.`;
	return `No workspace matches "${sanitizeTerminalText(query)}". Run /cloudthinker workspace to pick from the list.`;
}

export function loginNeeded(entry: WorkspaceEntry, origin: string): string {
	const where = entry.organizationName ? `${sanitizeTerminalText(entry.name)} in ${sanitizeTerminalText(entry.organizationName)}` : sanitizeTerminalText(entry.name);
	return `You are not logged in to ${where} yet. Run \`cloudthinker login --url ${origin}\`, choose it in the browser, then run /cloudthinker workspace again.`;
}

export async function workspaceCommand(
	runtime: CloudThinkerRuntime,
	ctx: ExtensionCommandContext,
	argument: string,
	deps: WorkspaceDeps = defaultDeps(),
): Promise<void> {
	const { ui } = ctx;
	if (deps.env.CLOUDTHINKER_TOKEN?.trim()) {
		ui.notify(TOKEN_PINS_WORKSPACE, "warning");
		return;
	}
	if (!ctx.isIdle()) {
		ui.notify("Wait for this turn to finish or stop it before changing workspace.", "warning");
		return;
	}
	let stored: StoredWorkspace[];
	try {
		stored = await listStoredWorkspaces(deps);
	} catch (error) {
		ui.notify(describeError(error), "error");
		return;
	}
	let remote: AgentCliWorkspace[] | undefined;
	try {
		remote = await runtime.client.listWorkspaces();
	} catch {
		remote = undefined;
	}
	const currentId =
		runtime.session?.workspace_id ??
		runtime.identity?.workspace_id ??
		deps.env[WORKSPACE_ENV_VAR]?.trim() ??
		stored.find((workspace) => workspace.active)?.id;
	const entries = buildEntries(stored, remote, currentId);
	let target: WorkspaceEntry | undefined;
	const query = argument.trim();
	if (query) {
		const match = matchWorkspace(entries, query);
		if (typeof match === "string") {
			ui.notify(match, "warning");
			return;
		}
		target = match;
	} else {
		if (entries.length < 2) {
			ui.notify(ONLY_ONE_WORKSPACE, "info");
			return;
		}
		const menu = workspaceMenu(entries, remote !== undefined);
		for (;;) {
			const choice = await ui.select("Switch workspace", menu.options);
			if (!choice) return;
			target = menu.byLabel.get(choice);
			if (target) break;
		}
	}
	if (target.id === currentId) {
		ui.notify(`Already in ${sanitizeTerminalText(target.name)}.`, "info");
		return;
	}
	if (!target.loggedIn) {
		ui.notify(loginNeeded(target, resolveBaseUrl(deps.env)), "warning");
		return;
	}

	const previous = deps.env[WORKSPACE_ENV_VAR];
	deps.env[WORKSPACE_ENV_VAR] = target.id;
	runtime.client.invalidateTokens();
	const failure = await registerProvider(runtime);
	if (failure) {
		if (previous === undefined) delete deps.env[WORKSPACE_ENV_VAR];
		else deps.env[WORKSPACE_ENV_VAR] = previous;
		runtime.client.invalidateTokens();
		ui.notify(`Could not switch to ${sanitizeTerminalText(target.name)}: ${failure}`, "error");
		return;
	}
	const saved = await deps.run(tokenBinary(deps.env), cliArgs(deps.env, "auth", "switch", target.id));
	const name = sanitizeTerminalText(target.name);
	const suffix = saved.status === 0 ? "" : " It is not saved as your default; run `cloudthinker auth switch` to keep it.";
	const result = await ctx.newSession({
		withSession: async (next) => {
			next.ui.notify(`Switched to ${name}. This is a new session in that workspace.${suffix}`, "info");
		},
	});
	if (result.cancelled) {
		ui.notify(`Switched to ${name}, but the new session was cancelled. Run /new to start in it.`, "warning");
	}
}
