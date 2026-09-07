import assert from "node:assert/strict";
import test from "node:test";

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

import { CloudThinkerClient, TokenSource } from "../src/client.ts";
import { CREDIT_GLYPH, CreditsMeter, formatCredits } from "../src/credits.ts";
import { CREDITS_KEY, CloudThinkerRuntime } from "../src/runtime.ts";
import { startFakeServer } from "./helpers.ts";

const CONVERSATION = "11111111-1111-4111-8111-111111111111";

function runtimeFor(origin: string): {
	runtime: CloudThinkerRuntime;
	statuses: { key: string; text: string | undefined }[];
} {
	const statuses: { key: string; text: string | undefined }[] = [];
	const client = new CloudThinkerClient({
		baseUrl: origin,
		tokens: new TokenSource({ CLOUDTHINKER_TOKEN: "t" }),
	});
	const runtime = new CloudThinkerRuntime({} as unknown as ExtensionAPI, client);
	runtime.bind({
		ui: {
			setStatus: (key: string, text: string | undefined) => {
				statuses.push({ key, text });
			},
		},
	} as never);
	runtime.session = {
		conversation_id: CONVERSATION,
		workspace_id: "w",
		web_url: "https://example.test/chat/1",
		auto_mode: { enabled: false, can_edit: false },
	};
	return { runtime, statuses };
}

test("credits read in credits, never in a currency", () => {
	assert.equal(formatCredits(0), `${CREDIT_GLYPH} 0 credits`);
	assert.equal(formatCredits(1), `${CREDIT_GLYPH} 1 credit`);
	assert.equal(formatCredits(1.754), `${CREDIT_GLYPH} 1.75 credits`);
	assert.equal(formatCredits(2048.5), `${CREDIT_GLYPH} 2,048.5 credits`);
	assert.ok(!formatCredits(3).includes("$"));
});

test("the meter reports the session's ledger total in the footer", async () => {
	const server = await startFakeServer((request) =>
		request.path === `/api/v1/agent-cli/sessions/${CONVERSATION}/credits`
			? { body: { credits_used: 1.75, tokens_consumed: 87_500 } }
			: undefined,
	);
	try {
		const { runtime, statuses } = runtimeFor(server.origin);

		await new CreditsMeter(runtime).read();

		assert.deepEqual(runtime.credits, { credits_used: 1.75, tokens_consumed: 87_500 });
		assert.deepEqual(statuses, [
			{ key: CREDITS_KEY, text: `${CREDIT_GLYPH} 1.75 credits` },
		]);
	} finally {
		await server.close();
	}
});

test("a session with no conversation yet reads nothing", async () => {
	const server = await startFakeServer(() => undefined);
	try {
		const { runtime, statuses } = runtimeFor(server.origin);
		runtime.session = undefined;

		await new CreditsMeter(runtime).read();

		assert.deepEqual(statuses, []);
		assert.equal(server.requests.length, 0);
	} finally {
		await server.close();
	}
});

test("a failed read leaves the last total standing", async () => {
	const server = await startFakeServer(() => ({ status: 503, body: { detail: "down" } }));
	try {
		const { runtime, statuses } = runtimeFor(server.origin);
		const meter = new CreditsMeter(runtime);

		meter.refresh();
		await new Promise((resolve) => setTimeout(resolve, 50));

		assert.deepEqual(statuses, []);
		assert.equal(runtime.credits, undefined);
	} finally {
		await server.close();
	}
});
