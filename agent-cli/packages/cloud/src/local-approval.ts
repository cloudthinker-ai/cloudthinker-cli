import { type ExtensionAPI, type ExtensionContext, isToolCallEventType, type ToolCallEvent } from "@earendil-works/pi-coding-agent";

import { subcommandName } from "./commands.ts";
import { terminalNotify } from "./notify.ts";
import type { AutoMode } from "./runtime.ts";
import { ApprovalPrompt } from "./tools/approval-prompt.ts";
import { firstLine } from "./tools/render.ts";

export const RUN_LOCALLY = "Run";
export const DECLINE_LOCALLY = "Decline";
export const LOCAL_APPROVAL_TITLE = "Manual mode: local change";
export const LOCAL_APPROVAL_NOTIFICATION = "CloudThinker: approval needed";
export const READ_ONLY_EXAMPLES = "ls, cat, rg, git status, kubectl get, aws describe/list/get";
export const DECLINED_REASON = "The user declined this local action. Ask how to proceed instead of retrying it.";

const LOCAL_OPTIONS = [RUN_LOCALLY, DECLINE_LOCALLY] as const;
const LOCAL_CHOICES = [
	{ label: RUN_LOCALLY, key: "y" },
	{ label: DECLINE_LOCALLY, key: "n" },
] as const;

const ANY_ARGUMENTS = "*";

const READ_ONLY_COMMANDS: Readonly<Record<string, readonly string[]>> = {
	ls: [ANY_ARGUMENTS], cat: [ANY_ARGUMENTS], head: [ANY_ARGUMENTS], tail: [ANY_ARGUMENTS], wc: [ANY_ARGUMENTS],
	grep: [ANY_ARGUMENTS], rg: [ANY_ARGUMENTS], fd: [ANY_ARGUMENTS], find: [ANY_ARGUMENTS], tree: [ANY_ARGUMENTS],
	pwd: [ANY_ARGUMENTS], cd: [ANY_ARGUMENTS], echo: [ANY_ARGUMENTS], which: [ANY_ARGUMENTS], stat: [ANY_ARGUMENTS],
	file: [ANY_ARGUMENTS], du: [ANY_ARGUMENTS], df: [ANY_ARGUMENTS], ps: [ANY_ARGUMENTS], uname: [ANY_ARGUMENTS],
	date: [ANY_ARGUMENTS], whoami: [ANY_ARGUMENTS], id: [ANY_ARGUMENTS], hostname: [ANY_ARGUMENTS],
	uptime: [ANY_ARGUMENTS], free: [ANY_ARGUMENTS], jq: [ANY_ARGUMENTS], sort: [ANY_ARGUMENTS], uniq: [ANY_ARGUMENTS],
	cut: [ANY_ARGUMENTS], tr: [ANY_ARGUMENTS], diff: [ANY_ARGUMENTS], journalctl: [ANY_ARGUMENTS],
	git: ["status", "diff", "log", "show", "rev-parse", "blame", "ls-files"],
	kubectl: ["get", "describe", "logs", "top", "version", "explain", "api-resources", "cluster-info"],
	helm: ["list", "status", "get", "history", "version"],
	terraform: ["plan", "show", "validate", "version", "output"],
	docker: ["ps", "images", "logs", "inspect", "version", "info"],
	systemctl: ["status", "list-units", "is-active"],
};

const DENIED_FLAGS: Readonly<Record<string, readonly string[]>> = {
	find: ["-exec", "-execdir", "-delete", "-ok", "-okdir", "-fprint", "-fprintf", "-fls"],
	sort: ["-o", "--output"],
};

const VALUE_FLAGS = new Set([
	"-n", "--namespace", "--context", "--kubeconfig", "-C", "-c", "--profile", "--region", "--output", "-o", "--project",
]);

function positional(rest: readonly string[]): string[] {
	const found: string[] = [];
	for (let index = 0; index < rest.length; index++) {
		const word = rest[index]!;
		if (word.startsWith("-")) {
			if (VALUE_FLAGS.has(word)) index++;
			continue;
		}
		found.push(word);
	}
	return found;
}

const SAFE_REDIRECTS = new Set([">/dev/null", "2>/dev/null", "&>/dev/null", "2>&1", ">&2", "1>&2"]);

function words(command: string): string[][] | undefined {
	const segments: string[][] = [[]];
	let word = "";
	let quote: "'" | '"' | undefined;
	const flush = () => {
		if (word) segments[segments.length - 1]!.push(word);
		word = "";
	};
	for (let index = 0; index < command.length; index++) {
		const char = command[index]!;
		if (quote) {
			if (char === quote) quote = undefined;
			else if (quote === '"' && (char === "`" || (char === "$" && command[index + 1] === "("))) return undefined;
			else word += char;
			continue;
		}
		if (char === "'" || char === '"') {
			quote = char;
			continue;
		}
		if (char === "`" || (char === "$" && command[index + 1] === "(")) return undefined;
		if (char === " " || char === "\t") {
			flush();
			continue;
		}
		if (char === "\n" || char === ";" || char === "|" || char === "&") {
			if (char === "&" && (command[index - 1] === ">" || command[index + 1] === ">")) {
				word += char;
				continue;
			}
			flush();
			segments.push([]);
			continue;
		}
		word += char;
	}
	if (quote) return undefined;
	flush();
	return segments.filter((segment) => segment.length > 0);
}

function readOnlySegment(segment: readonly string[]): boolean {
	const [program, ...rest] = segment;
	if (!program) return true;
	if (rest.some((word) => word.includes(">") && !SAFE_REDIRECTS.has(word))) return false;
	if (program.includes(">")) return false;
	const denied = DENIED_FLAGS[program] ?? [];
	if (rest.some((word) => denied.some((flag) => word === flag || word.startsWith(`${flag}=`)))) return false;
	if (program === "aws") {
		const operation = positional(rest)[1] ?? "";
		return /^(describe-|list-|get-)/.test(operation);
	}
	if (program === "gcloud") {
		const words = positional(rest);
		return ["list", "describe"].includes(words[words.length - 1] ?? "");
	}
	const allowed = READ_ONLY_COMMANDS[program];
	if (!allowed) return false;
	if (allowed.includes(ANY_ARGUMENTS)) return true;
	const verb = positional(rest)[0] ?? "";
	return allowed.includes(verb);
}

export function isReadOnlyCommand(command: string): boolean {
	const segments = words(command);
	return segments !== undefined && segments.length > 0 && segments.every(readOnlySegment);
}

interface LocalAction {
	subject: string;
	detail: string;
}

function localAction(event: ToolCallEvent): LocalAction | undefined {
	if (isToolCallEventType("bash", event)) {
		return isReadOnlyCommand(event.input.command) ? undefined : { subject: "Run a local command", detail: event.input.command };
	}
	if (isToolCallEventType("powershell", event)) return { subject: "Run a local PowerShell command", detail: event.input.command };
	if (isToolCallEventType("edit", event)) return { subject: "Edit a local file", detail: event.input.path };
	if (isToolCallEventType("write", event)) return { subject: "Write a local file", detail: event.input.path };
	return undefined;
}

export function headlessReason(subject: string): string {
	return `${subject} was blocked: the workspace Approval mode is Manual, so local commands that change something and file edits wait for a person, and this run has no terminal to ask. Run the agent interactively to approve it, or a workspace settings editor can switch to Auto with /${subcommandName("auto")} on.`;
}

async function askLocally(ctx: ExtensionContext, action: LocalAction): Promise<boolean> {
	const reasoning = `${action.subject}. Approval mode is Manual; read-only commands such as ${READ_ONLY_EXAMPLES} run without asking.`;
	if (ctx.mode === "tui") {
		terminalNotify(LOCAL_APPROVAL_NOTIFICATION);
		const choice = await ctx.ui.custom<string | undefined>((tui, theme, _keybindings, done) => new ApprovalPrompt({
			title: LOCAL_APPROVAL_TITLE,
			reasoning,
			place: "local",
			connections: "this machine",
			script: action.detail,
			webUrl: "",
			choices: LOCAL_CHOICES,
		}, theme, done, () => tui.requestRender(), () => tui.terminal.rows));
		return choice === RUN_LOCALLY;
	}
	const choice = await ctx.ui.select(`${action.subject}: ${firstLine(action.detail)}`, [...LOCAL_OPTIONS]);
	return choice === RUN_LOCALLY;
}

export function registerLocalApproval(pi: ExtensionAPI, mode: () => AutoMode | undefined): void {
	pi.on("tool_call", async (event: ToolCallEvent, ctx: ExtensionContext) => {
		if (mode()?.enabled !== false) return;
		const action = localAction(event);
		if (!action) return;
		if (!ctx.hasUI) return { block: true, reason: headlessReason(action.subject) };
		if (await askLocally(ctx, action)) return;
		return { block: true, reason: DECLINED_REASON };
	});
}
