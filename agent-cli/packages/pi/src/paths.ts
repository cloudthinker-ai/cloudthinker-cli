import { join } from "node:path";

import { getAgentDir } from "@earendil-works/pi-coding-agent";

import { isUuid } from "./uuid.ts";

export function cloudthinkerDir(agentDir: string = getAgentDir()): string {
	return join(agentDir, "cloudthinker");
}

export function outboxPath(
	conversationId: string,
	agentDir: string = getAgentDir(),
): string {
	return join(agentDir, "outbox", `${conversationId}.jsonl`);
}

export function rejectedPath(
	conversationId: string,
	agentDir: string = getAgentDir(),
): string {
	return join(agentDir, "outbox", `${conversationId}.rejected.json`);
}

function workspaceSegment(workspaceId: string): string {
	if (!isUuid(workspaceId)) {
		throw new Error(`Workspace id "${workspaceId}" is not a UUID, so it cannot name a skills directory`);
	}
	return workspaceId;
}

export function skillsRoot(
	workspaceId: string,
	agentDir: string = getAgentDir(),
): string {
	return join(cloudthinkerDir(agentDir), "skills", workspaceSegment(workspaceId));
}

export function skillsIndexPath(
	workspaceId: string,
	agentDir: string = getAgentDir(),
): string {
	return join(cloudthinkerDir(agentDir), "skills", `${workspaceSegment(workspaceId)}.json`);
}
