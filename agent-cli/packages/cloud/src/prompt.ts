import type { CloudThinkerRuntime } from "./runtime.ts";
import { MEMORY_DIR, SANDBOX_HOME } from "./memory.ts";
import { CT_ASK, CT_SANDBOX_READ, CT_SANDBOX_WRITE, CT_RUN_STATUS, LOAD_SKILL, TOOL_SEARCH } from "./tools/names.ts";

export const AUTO_MODE_LINE =
	"Workspace approval mode: Auto. A write runs at once when the workspace rule allows it and pauses otherwise.";
export const MANUAL_MODE_LINE = "Workspace approval mode: Manual. Every cloud write, local file edit and local command that changes something pauses for a human; read-only local commands run at once.";

export const MENTION_GUIDANCE = "Mentions in a user message: `#connection/<prefix>` or `#connection/<prefix>/<alias>` names the Connection the request is about; `@` followed by a relative path, such as `@./src/app.ts` or `@../notes.md`, names a file on this machine; a bare `@name` names an agent.";

export function approvalModeLine(enabled: boolean): string {
	return enabled ? AUTO_MODE_LINE : MANUAL_MODE_LINE;
}

const BLOCK_TAG = /<\s*(\/?)\s*(cloudthinker|memory_index|user_notes|connections_context)\s*>/gi;
const CONNECTIONS_WRAPPER = /^(\s*<connections_context>)([\s\S]*)(<\/connections_context>\s*)$/;

export function neutralizeBlockTags(text: string): string {
	return text.replace(BLOCK_TAG, (_match, close: string, tag: string) => `&lt;${close}${tag}&gt;`);
}

export function guardedConnectionsXml(xml: string): string {
	const wrapped = CONNECTIONS_WRAPPER.exec(xml);
	if (!wrapped) return neutralizeBlockTags(xml);
	return `${wrapped[1]}${neutralizeBlockTags(wrapped[2] ?? "")}${wrapped[3]}`;
}

export const CONNECTION_SKILL_LINE = [
	`Read the relevant advertised Connection guides with ${LOAD_SKILL} and use their scripts when available.`,
	`For an SDK Connection with no named guides, call ${LOAD_SKILL} with just its connection_prefix to get its tool policy.`,
	`Use ${TOOL_SEARCH} to find SDK tools by task or to load schemas by exact name (prefix.tool).`,
	"Import a Connection tool from @connections/<prefix> in a TypeScript script and run it with Bun on the workspace machine. Pass the family prefix in connection_list and the instance alias in the SDK call when the Connection lists multiple instances.",
].join(" ");

const CYBER_OPERATING_GUIDANCE = "For Cyber work, read `cloudthinker --skill cyber`, use `ct_workflow` for staged execution, and bind each run to this session's conversation id.";
export const FINISH_GUIDANCE = "Finish the task before you end your turn. When you say you will take a step, take it in the same reply with a tool call. End your turn only when the work is done and checked, or when you need the user's decision; then name the concrete blocker or question.";
const LOCAL_SEARCH_GUIDANCE = "For local shell searches, use `rg` for file contents and `fd` for file and directory names. Scope searches to the relevant paths and keep ignore rules enabled. Use `grep` or `find` for these searches only if the corresponding preferred command is unavailable. Before falling back, confirm unavailability with `command -v rg` or `command -v fd`; an empty search result is not unavailability.";

export function sandboxLayout(runtime: CloudThinkerRuntime): string {
	if (!runtime.session) return "";
	return ` Your shell there opens in ${SANDBOX_HOME}/${runtime.session.conversation_id}, this session's own directory, whose \`.memory\`, \`_skills\` and \`_connections\` entries are symlinks up into the shared workspace tree at ${SANDBOX_HOME}. Name a file on the workspace machine by absolute path; a relative one resolves inside that session directory, not in the workspace tree. Put every scratch file under ${SANDBOX_HOME}/${runtime.session.conversation_id}/tmp, never in ${SANDBOX_HOME} itself.`;
}

export function buildPromptBlock(runtime: CloudThinkerRuntime): string {
	if (!runtime.cloudEnabled) {
		return `<cloudthinker>\nYou are the \`cloudthinker agent\` running in the developer's terminal. Cloud is off, so workspace tools and CloudThinker delegation are unavailable; local tools remain available. The user can enable Cloud with /cloud on.\n${FINISH_GUIDANCE}\n${LOCAL_SEARCH_GUIDANCE}\n</cloudthinker>`;
	}
	const lines: string[] = [];
	if (runtime.identity) {
		lines.push(
			neutralizeBlockTags(
				`Workspace: ${runtime.identity.workspace_name} (id ${runtime.identity.workspace_id}). You are signed in as ${runtime.identity.user_email}.`,
			),
		);
	}
	lines.push(
		"You are the `cloudthinker agent` running in the developer's terminal. The bundled pi documentation applies.",
		"You work in exactly two environments:",
		"1. This machine: the developer's own computer and the current directory. Local tools (bash, read, write, edit, grep, find, ls) run here. It holds NO cloud credential, and you never ask the user for one.",
		`2. The workspace machine: CloudThinker's machine in the cloud that the workspace owns. Only ${CT_SANDBOX_READ} and ${CT_SANDBOX_WRITE} run there, with a workspace Connection's credential injected for the run; the credential never leaves the cloud and never reaches this machine.${sandboxLayout(runtime)}`,
		`Local tools use this machine's filesystem; workspace tools use the cloud filesystem. Paths returned by an API are not necessarily local files or mounts.`,
		FINISH_GUIDANCE,
		LOCAL_SEARCH_GUIDANCE,
		MENTION_GUIDANCE,
		`Connected workspace Connections: ${neutralizeBlockTags(runtime.connectedPrefixes.join(", ")) || "none"}. A Connection is a credential the workspace machine can use, not a third environment.`,
	);
	if (runtime.connections.xml) {
		lines.push(guardedConnectionsXml(runtime.connections.xml), CONNECTION_SKILL_LINE);
	}
	lines.push(
		`For anything that needs one of those, use its tool policy to choose a workspace command. Skip disabled tools. For a read-only command that needs no approval, call ${CT_SANDBOX_READ} with connection_list drawn ONLY from that list.`,
		`For ONE state-changing cloud command or a Connection tool marked approval-required, call ${CT_SANDBOX_WRITE}. The workspace applies its approval rules and may pause for a decision in the terminal or browser. Use that approval prompt for confirmation.`,
	);
	if (runtime.autoMode) lines.push(approvalModeLine(runtime.autoMode.enabled));
	lines.push(`Use ${CT_SANDBOX_READ} for bounded cloud investigations.`);
	lines.push("Use list_connections for workspace Connection inventory, including Failed Connections. For a health check, call test_workspace_connection for each requested Connection and report what Test returns. Use these tools when available.");
	lines.push(
		`Call ${CT_ASK} for workspace memory or several dependent steps; delegated work runs on the workspace machine, human approval is required when requested, and ${CT_RUN_STATUS} resumes a paused run.`,
	);
	lines.push(
		`A workspace skill may name tools or paths that only exist on the workspace machine. Run those steps there using the read and write rules above.`,
		"When asked who you are or what you can reach, answer with these two environments and that Connection list.",
	);
	if (runtime.session) {
		lines.push(`This session is mirrored to ${runtime.session.web_url}.`);
		lines.push(
			`This session's CloudThinker conversation id is ${runtime.session.conversation_id}. ${CYBER_OPERATING_GUIDANCE}`,
		);
	}
	const blocks = [`<cloudthinker>\n${lines.join("\n")}\n</cloudthinker>`];
	if (runtime.memory?.memoryIndex) {
		blocks.push(
			`<memory_index>\n${neutralizeBlockTags(runtime.memory.memoryIndex)}\n</memory_index>`,
			`The memory index is read-only here and was read once when this session started. It indexes ${MEMORY_DIR}/ on the workspace machine; open a named file with ${CT_SANDBOX_READ} (\`cat ${MEMORY_DIR}/<path>\`, no Connection needed). CloudThinker Agent in the cloud writes there through ${CT_ASK}.`,
		);
	}
	if (runtime.memory?.userNotes) {
		blocks.push(`<user_notes>\n${neutralizeBlockTags(runtime.memory.userNotes)}\n</user_notes>`);
	}
	return blocks.join("\n");
}

export function appendPromptBlock(systemPrompt: string, block: string): string {
	return `${systemPrompt}\n\n${block}`;
}
