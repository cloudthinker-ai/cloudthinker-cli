import type { ExtensionContext } from "@earendil-works/pi-coding-agent";

import { PROVIDER_ID, applyConversationHeader } from "@cloudthinker/pi/src/provider.ts";
import { findLinkedSession } from "@cloudthinker/pi/src/session.ts";

export const BTW_NEEDS_CONVERSATION = "/btw answers from this conversation, which is not linked to CloudThinker yet. Send a message first, then ask again.";

export function btwRequestHeaders(
	model: { provider: string },
	ctx: Pick<ExtensionContext, "sessionManager">,
): Record<string, string> {
	const conversationId = findLinkedSession(ctx.sessionManager.getEntries())?.conversation_id;
	if (model.provider === PROVIDER_ID && !conversationId) throw new Error(BTW_NEEDS_CONVERSATION);
	const headers: Record<string, string | null> = {};
	applyConversationHeader(headers, model.provider, conversationId);
	return headers as Record<string, string>;
}
