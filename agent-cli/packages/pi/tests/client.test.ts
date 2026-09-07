import assert from "node:assert/strict";
import test from "node:test";

import {
	CloudThinkerApiError,
	CloudThinkerClient,
	DEFAULT_BASE_URL,
	TokenSource,
	resolveBaseUrl,
	tokenCommandArgs,
} from "../src/client.ts";
import { startFakeServer } from "./helpers.ts";

test("base url falls back to the hosted origin and drops a trailing slash", () => {
	assert.equal(resolveBaseUrl({}), DEFAULT_BASE_URL);
	assert.equal(resolveBaseUrl({ CLOUDTHINKER_URL: "  " }), DEFAULT_BASE_URL);
	assert.equal(
		resolveBaseUrl({ CLOUDTHINKER_URL: "http://localhost:9900//" }),
		"http://localhost:9900",
	);
});

test("the api url appends /api/v1 to the bare origin", () => {
	const client = new CloudThinkerClient({ baseUrl: "http://localhost:9900" });
	assert.equal(client.apiUrl, "http://localhost:9900/api/v1");
});

test("an env token wins and the token command never runs", () => {
	let calls = 0;
	const tokens = new TokenSource({ CLOUDTHINKER_TOKEN: "env-value" }, () => {
		calls += 1;
		return { status: 0, stdout: "command-value" };
	});
	assert.equal(tokens.resolve(), "env-value");
	assert.equal(tokens.fromEnvironment, true);
	assert.equal(calls, 0);
});

test("without an env token the command runs once and is cached until invalidated", () => {
	let calls = 0;
	const tokens = new TokenSource({}, () => {
		calls += 1;
		return { status: 0, stdout: `command-value-${calls}\n` };
	});
	assert.equal(tokens.resolve(), "command-value-1");
	assert.equal(tokens.resolve(), "command-value-1");
	assert.equal(calls, 1);
	tokens.invalidate();
	assert.equal(tokens.resolve(), "command-value-2");
	assert.equal(calls, 2);
});

test("the token command carries the workspace the wrapper pinned", () => {
	assert.deepEqual(tokenCommandArgs({}), ["auth", "token"]);
	assert.deepEqual(tokenCommandArgs({ CLOUDTHINKER_WORKSPACE: "  " }), ["auth", "token"]);
	assert.deepEqual(tokenCommandArgs({ CLOUDTHINKER_WORKSPACE: "ws-1" }), [
		"auth",
		"token",
		"--workspace",
		"ws-1",
	]);

	let seen: string[] = [];
	const tokens = new TokenSource({ CLOUDTHINKER_WORKSPACE: "ws-1" }, (_command, args) => {
		seen = args;
		return { status: 0, stdout: "value" };
	});
	tokens.resolve();
	assert.deepEqual(seen, ["auth", "token", "--workspace", "ws-1"]);
});

test("a failing token command raises 401 and quotes no command output", () => {
	const tokens = new TokenSource({}, () => ({ status: 3, stdout: "leaky-value" }));
	let raised: unknown;
	try {
		tokens.resolve();
	} catch (error) {
		raised = error;
	}
	assert.ok(raised instanceof CloudThinkerApiError);
	assert.equal(raised.status, 401);
	assert.ok(!raised.message.includes("leaky-value"));
});

test("the bearer reaches the server and a 401 retries once on a fresh token", async () => {
	let calls = 0;
	const tokens = new TokenSource({}, () => {
		calls += 1;
		return { status: 0, stdout: `token-${calls}` };
	});
	const server = await startFakeServer((request) => {
		if (request.headers.authorization === "Bearer token-1") return { status: 401 };
		return {
			body: {
				conversation_id: "c-1",
				workspace_id: "w-1",
				web_url: "http://web/c-1",
			},
		};
	});
	try {
		const client = new CloudThinkerClient({ baseUrl: server.origin, tokens });
		const created = await client.createSession({ cwd: "/tmp/repo" });
		assert.equal(created.conversation_id, "c-1");
		assert.equal(server.requests.length, 2);
		assert.equal(server.requests[1]?.headers.authorization, "Bearer token-2");
	} finally {
		await server.close();
	}
});

test("an error body is read from error.message, then detail", async () => {
	const server = await startFakeServer((request) => {
		if (request.path.startsWith("/api/v1/agent-cli/executions")) {
			return {
				status: 422,
				body: {
					error: { code: "x", message: "Connections not available: k8s" },
					detail: "legacy",
				},
			};
		}
		return { status: 500, body: { detail: "boom" } };
	});
	try {
		const client = new CloudThinkerClient({
			baseUrl: server.origin,
			tokens: new TokenSource({ CLOUDTHINKER_TOKEN: "t" }),
		});
		const fromError: unknown = await client
			.execute({
				conversation_id: "c-1",
				connection_list: ["k8s"],
				script: "true",
				timeout: 5,
				run_in_background: false,
			})
			.catch((error: unknown) => error);
		assert.ok(fromError instanceof CloudThinkerApiError);
		assert.equal(fromError.status, 422);
		assert.equal(fromError.message, "Connections not available: k8s");

		const fromDetail: unknown = await client
			.whoami()
			.catch((error: unknown) => error);
		assert.ok(fromDetail instanceof CloudThinkerApiError);
		assert.equal(fromDetail.status, 500);
		assert.equal(fromDetail.message, "boom");
	} finally {
		await server.close();
	}
});

test("the connections context carries chat's xml and the prefix allowlist as the server built them", async () => {
	const server = await startFakeServer((request) =>
		request.path === "/api/v1/agent-cli/connections"
			? { body: { xml: "<connections_context>\n<aws/>\n</connections_context>", prefixes: ["aws", "k8s"] } }
			: undefined,
	);
	try {
		const client = new CloudThinkerClient({
			baseUrl: server.origin,
			tokens: new TokenSource({ CLOUDTHINKER_TOKEN: "t" }),
		});
		assert.deepEqual(await client.getConnectionsContext(), {
			xml: "<connections_context>\n<aws/>\n</connections_context>",
			prefixes: ["aws", "k8s"],
		});
	} finally {
		await server.close();
	}
});

test("the auto-mode switch patches the workspace and the notify resend names the conversation", async () => {
	const server = await startFakeServer((request) => {
		if (request.path === "/api/v1/workspaces/w-1/auto-mode" && request.method === "PATCH") {
			return { body: { enabled: true, updated_at: "2026-09-07T00:00:00Z", updated_by: "u-1" } };
		}
		if (request.path === "/api/v1/notifications/interrupt/resend?conversation_id=h-1") {
			return { body: { message: "sent" } };
		}
		return undefined;
	});
	try {
		const client = new CloudThinkerClient({
			baseUrl: server.origin,
			tokens: new TokenSource({ CLOUDTHINKER_TOKEN: "t" }),
		});
		const status = await client.setWorkspaceAutoMode("w-1", true);
		assert.equal(status.enabled, true);
		assert.deepEqual(server.requests[0]?.body, { enabled: true });
		await client.resendInterruptNotification("h-1");
		assert.equal(server.requests[1]?.method, "POST");
	} finally {
		await server.close();
	}
});

test("a transport failure is a status 0 error naming the origin, never a bare throw", async () => {
	const client = new CloudThinkerClient({
		baseUrl: "http://127.0.0.1:9",
		tokens: new TokenSource({ CLOUDTHINKER_TOKEN: "t" }),
		fetchImpl: async () => {
			throw new DOMException("The operation was aborted.", "AbortError");
		},
	});
	const error: unknown = await client.whoami().catch((raised: unknown) => raised);
	assert.ok(error instanceof CloudThinkerApiError);
	assert.equal(error.status, 0);
	assert.match(error.message, /^Could not reach http:\/\/127\.0\.0\.1:9: /);
	assert.match(error.message, /aborted/);
});

test("a non-JSON error body falls back to the status line", async () => {
	const server = await startFakeServer(() => ({
		status: 502,
		raw: Buffer.from("<html>bad gateway</html>"),
	}));
	try {
		const client = new CloudThinkerClient({
			baseUrl: server.origin,
			tokens: new TokenSource({ CLOUDTHINKER_TOKEN: "t" }),
		});
		const error: unknown = await client.whoami().catch((raised: unknown) => raised);
		assert.ok(error instanceof CloudThinkerApiError);
		assert.equal(error.status, 502);
		assert.equal(error.message, "502 Bad Gateway");
	} finally {
		await server.close();
	}
});
