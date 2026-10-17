export function isSafeConversationId(value: unknown): value is string {
	return typeof value === "string" && /^[A-Za-z0-9_-]{1,128}$/.test(value);
}

export function conversationPathSegment(conversationId: string): string {
	if (!isSafeConversationId(conversationId)) {
		throw new Error("Conversation ID must contain only letters, digits, hyphens, and underscores");
	}
	return encodeURIComponent(conversationId);
}
