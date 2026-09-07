import { createServer, type IncomingMessage, type Server } from "node:http";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { AddressInfo } from "node:net";

export interface RecordedRequest {
	method: string;
	path: string;
	headers: Record<string, string | string[] | undefined>;
	body: unknown;
}

export type Route = (
	request: RecordedRequest,
) => { status?: number; body?: unknown; raw?: Buffer } | undefined;

export interface FakeServer {
	origin: string;
	requests: RecordedRequest[];
	close(): Promise<void>;
}

async function readBody(request: IncomingMessage): Promise<unknown> {
	const chunks: Buffer[] = [];
	for await (const chunk of request) chunks.push(chunk as Buffer);
	if (chunks.length === 0) return undefined;
	try {
		return JSON.parse(Buffer.concat(chunks).toString("utf8"));
	} catch {
		return Buffer.concat(chunks).toString("utf8");
	}
}

export async function startFakeServer(route: Route): Promise<FakeServer> {
	const requests: RecordedRequest[] = [];
	const server: Server = createServer((request, response) => {
		void (async () => {
			const recorded: RecordedRequest = {
				method: request.method ?? "GET",
				path: request.url ?? "/",
				headers: request.headers,
				body: await readBody(request),
			};
			requests.push(recorded);
			const answer = route(recorded);
			if (!answer) {
				response.writeHead(404, { "content-type": "application/json" });
				response.end(JSON.stringify({ detail: "no route" }));
				return;
			}
			if (answer.raw) {
				response.writeHead(answer.status ?? 200, {
					"content-type": "application/zip",
				});
				response.end(answer.raw);
				return;
			}
			response.writeHead(answer.status ?? 200, { "content-type": "application/json" });
			response.end(JSON.stringify(answer.body ?? {}));
		})();
	});
	await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
	const address = server.address() as AddressInfo;
	return {
		origin: `http://127.0.0.1:${address.port}`,
		requests,
		close: () =>
			new Promise<void>((resolve, reject) =>
				server.close((error) => (error ? reject(error) : resolve())),
			),
	};
}

export async function withTempDir<T>(work: (dir: string) => Promise<T>): Promise<T> {
	const dir = await mkdtemp(join(tmpdir(), "ct-pi-test-"));
	try {
		return await work(dir);
	} finally {
		await rm(dir, { recursive: true, force: true });
	}
}
