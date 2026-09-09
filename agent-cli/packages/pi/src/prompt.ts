import type { CloudThinkerRuntime } from "./runtime.ts";
import { MEMORY_DIR, SANDBOX_HOME } from "./memory.ts";
import { CT_ASK, CT_SANDBOX_READ, CT_SANDBOX_WRITE, CT_RUN_STATUS } from "./tools/names.ts";

export const AUTO_MODE_LINE =
	"Workspace approval mode: Auto. A write runs at once when the workspace rule allows it and pauses otherwise.";
export const MANUAL_MODE_LINE = "Workspace approval mode: Manual. Every write pauses for a human.";

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

export const CONNECTION_SKILLS_DIR = `${SANDBOX_HOME}/_skills/connections`;
export const CONNECTION_SKILL_LINE = `A \`skill:\` line inside a Connection names that Connection's guide, a SKILL.md in the Sandbox. Before your first command on a Connection, read its guide with ${CT_SANDBOX_READ} and an empty connection_list (\`cat ${CONNECTION_SKILLS_DIR}/*/<skill>/SKILL.md\`), then follow it: its scripts run in the Sandbox through ${CT_SANDBOX_READ} with that Connection, so do not write your own script for what the guide already provides.`;

export function sandboxLayout(runtime: CloudThinkerRuntime): string {
	if (!runtime.session) return "";
	return ` Your shell there opens in ${SANDBOX_HOME}/${runtime.session.conversation_id}, this session's own directory, whose \`.memory\`, \`_skills\` and \`_connections\` entries are symlinks up into the shared workspace tree at ${SANDBOX_HOME}. Name a Sandbox file by absolute path; a relative one resolves inside that session directory, not in the workspace tree. Put every scratch file under ${SANDBOX_HOME}/${runtime.session.conversation_id}/tmp, never in ${SANDBOX_HOME} itself.`;
}

export function buildPromptBlock(runtime: CloudThinkerRuntime): string {
	const lines: string[] = [];
	if (runtime.identity) {
		lines.push(
			neutralizeBlockTags(
				`Workspace: ${runtime.identity.workspace_name} (id ${runtime.identity.workspace_id}). You are signed in as ${runtime.identity.user_email}.`,
			),
		);
	}
	lines.push(
		"You are CloudThinker Agent, the `cloudthinker agent` command, running in the developer's terminal. You are built on pi by Mario Zechner; the bundled docs describe pi and apply here.",
		"You work in exactly two environments:",
		"1. This machine: the developer's own computer and the current directory. Local tools (bash, read, write, edit, grep, find, ls) run here. It holds NO cloud credential, and you never ask the user for one.",
		`2. The CloudThinker Sandbox: a machine the workspace owns in the cloud. Only ${CT_SANDBOX_READ} and ${CT_SANDBOX_WRITE} run there, with a workspace Connection's credential injected for the run; the credential never leaves the cloud and never reaches this machine.${sandboxLayout(runtime)}`,
		`Connected workspace Connections: ${neutralizeBlockTags(runtime.connectedPrefixes.join(", ")) || "none"}. A Connection is a credential the Sandbox can use, not a third environment.`,
	);
	if (runtime.connections.xml) {
		lines.push(guardedConnectionsXml(runtime.connections.xml), CONNECTION_SKILL_LINE);
	}
	lines.push(
		`For anything that needs one of those, call ${CT_SANDBOX_READ} with connection_list drawn ONLY from that list; it runs in the CloudThinker Sandbox and returns stdout.`,
		`For ONE state-changing cloud command, call ${CT_SANDBOX_WRITE}; the workspace runs it at once or pauses it for a human to approve, in this terminal or in the browser. That approval is the confirmation, so do not ask for one in chat first.`,
	);
	if (runtime.autoMode) lines.push(approvalModeLine(runtime.autoMode.enabled));
	lines.push(
		`Do bounded cloud investigations yourself with ${CT_SANDBOX_READ}. Call ${CT_ASK} when the work needs what only the workspace has: its memory of past incidents and decisions, a specialist agent, or a change of several dependent steps; Anna works there with the same Sandbox, a human approves her writes in the browser, and ${CT_RUN_STATUS} resumes a paused run.`,
		`A workspace skill written for the CloudThinker Sandbox may name tools or paths that do not exist on this machine; run any step of it that needs a Connection through ${CT_SANDBOX_READ}.`,
		"When asked who you are or what you can reach, answer with these two environments and that Connection list.",
	);
	if (runtime.session) {
		lines.push(`This session is mirrored to ${runtime.session.web_url}.`);
	}
	const blocks = [`<cloudthinker>\n${lines.join("\n")}\n</cloudthinker>`];
	if (runtime.memory?.memoryIndex) {
		blocks.push(
			`<memory_index>\n${neutralizeBlockTags(runtime.memory.memoryIndex)}\n</memory_index>`,
			`The memory index is read-only here and was read once when this session started. It indexes ${MEMORY_DIR}/ in the Sandbox, so open any file it names with ${CT_SANDBOX_READ} (\`cat ${MEMORY_DIR}/<path>\`, no Connection needed). Only Anna writes there, through ${CT_ASK}.`,
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
