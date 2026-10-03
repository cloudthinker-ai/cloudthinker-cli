import { singleFlight } from "./async.ts";
import type { SessionCredits } from "./client.ts";
import type { CloudThinkerRuntime } from "./runtime.ts";

export const CREDIT_GLYPH = "◆";

export function formatCredits(credits: number): string {
	const rounded = Math.round(credits * 100) / 100;
	const text = rounded.toLocaleString("en-US", { maximumFractionDigits: 2 });
	return `${CREDIT_GLYPH} ${text} ${rounded === 1 ? "credit" : "credits"}`;
}

export class CreditsMeter {
	private readonly runtime: CloudThinkerRuntime;
	readonly read: () => Promise<void>;

	constructor(runtime: CloudThinkerRuntime) {
		this.runtime = runtime;
		this.read = singleFlight(() => this.readOnce());
	}

	refresh(): void {
		void this.read().catch(() => {});
	}

	private async readOnce(): Promise<void> {
		const session = this.runtime.session;
		if (!session) return;
		const credits = await this.runtime.client.readCredits(session.conversation_id);
		this.runtime.credits = credits;
		this.runtime.setCredits(formatCredits(credits.credits_used));
	}
}

export type { SessionCredits };
