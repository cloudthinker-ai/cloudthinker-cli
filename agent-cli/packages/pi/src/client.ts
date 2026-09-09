import { spawnSync } from "node:child_process";

export const DEFAULT_BASE_URL = "https://app.cloudthinker.io";
export const TOKEN_COMMAND = "cloudthinker auth token";

export class CloudThinkerApiError extends Error {
	readonly status: number;

	constructor(status: number, message: string) {
		super(message);
		this.name = "CloudThinkerApiError";
		this.status = status;
	}
}

export interface CommandResult {
	status: number | null;
	stdout: string;
}

export type CommandRunner = (command: string, args: string[]) => CommandResult;

const runCommand: CommandRunner = (command, args) => {
	const result = spawnSync(command, args, { encoding: "utf8", timeout: 20_000 });
	return { status: result.status, stdout: result.stdout ?? "" };
};

export function tokenCommandArgs(env: NodeJS.ProcessEnv = process.env): string[] {
	const workspace = env.CLOUDTHINKER_WORKSPACE?.trim();
	return workspace ? ["auth", "token", "--workspace", workspace] : ["auth", "token"];
}

export function resolveBaseUrl(env: NodeJS.ProcessEnv = process.env): string {
	const configured = env.CLOUDTHINKER_URL?.trim();
	const origin = configured && configured.length > 0 ? configured : DEFAULT_BASE_URL;
	return origin.replace(/\/+$/, "");
}

export class TokenSource {
	private cached: string | undefined;
	private readonly env: NodeJS.ProcessEnv;
	private readonly run: CommandRunner;

	constructor(env: NodeJS.ProcessEnv = process.env, run: CommandRunner = runCommand) {
		this.env = env;
		this.run = run;
	}

	get fromEnvironment(): boolean {
		return (this.env.CLOUDTHINKER_TOKEN?.trim() ?? "").length > 0;
	}

	resolve(): string {
		const configured = this.env.CLOUDTHINKER_TOKEN?.trim();
		if (configured) return configured;
		if (this.cached) return this.cached;
		const args = tokenCommandArgs(this.env);
		const result = this.run("cloudthinker", args);
		const token = result.stdout.trim();
		if (result.status !== 0 || token.length === 0) {
			throw new CloudThinkerApiError(
				401,
				`\`cloudthinker ${args.join(" ")}\` exited with code ${result.status ?? "null"}. Run \`cloudthinker login\`.`,
			);
		}
		this.cached = token;
		return token;
	}

	invalidate(): void {
		this.cached = undefined;
	}
}

export interface AutoModeStatus {
	enabled: boolean;
	can_edit: boolean;
}

export interface SessionCreated {
	conversation_id: string;
	workspace_id: string;
	web_url: string;
	auto_mode: AutoModeStatus;
}

export interface MirrorEntryInput {
	entry_id: string;
	parent_id: string | null;
	entry_type: string;
	payload: unknown;
}

export interface EntriesAppended {
	stored: number;
	last_seq: number;
}

export interface MirrorEntry extends MirrorEntryInput {
	seq: number;
}

export interface EntryPage {
	entries: MirrorEntry[];
	last_seq: number;
}

export interface ExecutionRequest {
	conversation_id: string;
	connection_list: string[];
	script: string;
	timeout: number;
	run_in_background: boolean;
}

export type ExecutionResult =
	| { status: "completed"; return_code: number; stdout: string; stderr: string }
	| { status: "running"; task_id: string };

export type WriteStatus =
	| "required_approval"
	| "approved"
	| "declined"
	| "denied"
	| "executed"
	| "failed";

export interface WriteRequest {
	conversation_id: string;
	tool_call_id: string;
	connection_list: string[];
	script: string;
	reasoning: string;
	timeout: number;
	run_in_background: boolean;
	recent_user_messages: string[];
}

export type WriteVerdict = "allow" | "require_approval" | "escalate" | "hard_deny";

export interface CloudWrite {
	id: string;
	conversation_id: string;
	tool_call_id: string;
	connection_list: string[];
	script: string;
	reasoning: string;
	verdict: WriteVerdict;
	verdict_reason: string;
	status: WriteStatus;
	trusted: boolean;
	decided_by_name: string | null;
	decline_reason: string | null;
	task_id: string | null;
	return_code: number | null;
	expires_at: string;
	web_url: string;
}

export interface WriteOutcome {
	write: CloudWrite;
	execution: ExecutionResult | null;
}

export type ExecutionOutputStatus = "running" | "done" | "error" | "cancelled" | "unknown";

export interface ExecutionOutput {
	status: ExecutionOutputStatus;
	output: string;
	next_cursor: number;
	truncated: boolean;
	exit_code?: number | null;
	termination_reason?: string | null;
}

export interface GatewayModel {
	id: string;
	name: string;
	reasoning: boolean;
	contextWindow: number;
	maxTokens: number;
	input: ("text" | "image")[];
}

export interface SessionCredits {
	credits_used: number;
	tokens_consumed: number;
}

export interface Identity {
	user_email: string;
	workspace_id: string;
	workspace_name: string;
	organization_id: string | null;
}

export interface ConnectionsContext {
	xml: string;
	prefixes: string[];
}

export interface WorkspaceAutoMode {
	enabled: boolean;
}

export type SkillContentStatus = "available" | "missing" | "invalid" | "unknown";

export interface WorkspaceSkill {
	name: string;
	description: string;
	enabled: boolean;
	updated_at: string;
	content_status: SkillContentStatus;
}

export type RunStatus =
	| "pending"
	| "running"
	| "succeeded"
	| "failed"
	| "required_approval";

export interface RunSubmitted {
	run_id: string;
	conversation_id: string;
	status: RunStatus;
	web_url: string;
}

export interface RunState {
	run_id: string;
	conversation_id: string | null;
	status: RunStatus;
	answer: string | null;
	message: string | null;
	failure_kind: string | null;
	web_url: string | null;
}

interface CallOptions {
	method: string;
	path: string;
	body?: unknown;
	query?: Record<string, string | number | undefined>;
	timeoutMs: number;
	signal?: AbortSignal;
}

const DEFAULT_TIMEOUT_MS = 30_000;
const SESSION_CREATE_TIMEOUT_MS = 90_000;

export class CloudThinkerClient {
	readonly baseUrl: string;
	private readonly tokens: TokenSource;
	private readonly fetchImpl: typeof fetch;

	constructor(options: {
		baseUrl?: string;
		tokens?: TokenSource;
		fetchImpl?: typeof fetch;
	} = {}) {
		this.baseUrl = options.baseUrl ?? resolveBaseUrl();
		this.tokens = options.tokens ?? new TokenSource();
		this.fetchImpl = options.fetchImpl ?? fetch;
	}

	get apiUrl(): string {
		return `${this.baseUrl}/api/v1`;
	}

	createSession(body: {
		cwd: string;
		title?: string;
		source_conversation_id?: string;
	}): Promise<SessionCreated> {
		return this.json({
			method: "POST",
			path: "/agent-cli/sessions",
			body,
			timeoutMs: SESSION_CREATE_TIMEOUT_MS,
		});
	}

	appendEntries(
		conversationId: string,
		entries: MirrorEntryInput[],
	): Promise<EntriesAppended> {
		return this.json({
			method: "PUT",
			path: `/agent-cli/sessions/${conversationId}/entries`,
			body: { entries },
			timeoutMs: 60_000,
		});
	}

	listEntries(
		conversationId: string,
		query: { after_seq?: number; limit?: number } = {},
	): Promise<EntryPage> {
		return this.json({
			method: "GET",
			path: `/agent-cli/sessions/${conversationId}/entries`,
			query,
			timeoutMs: DEFAULT_TIMEOUT_MS,
		});
	}

	execute(body: ExecutionRequest, signal?: AbortSignal): Promise<ExecutionResult> {
		return this.json({
			method: "POST",
			path: "/agent-cli/executions",
			body,
			timeoutMs: body.timeout * 1000 + 30_000,
			signal,
		});
	}

	readExecution(
		taskId: string,
		query: { conversation_id: string; since?: number },
		signal?: AbortSignal,
	): Promise<ExecutionOutput> {
		return this.json({
			method: "GET",
			path: `/agent-cli/executions/${encodeURIComponent(taskId)}`,
			query,
			timeoutMs: DEFAULT_TIMEOUT_MS,
			signal,
		});
	}

	requestWrite(body: WriteRequest, signal?: AbortSignal): Promise<WriteOutcome> {
		return this.json({
			method: "POST",
			path: "/agent-cli/writes",
			body,
			timeoutMs: body.timeout * 1000 + 60_000,
			signal,
		});
	}

	getWrite(writeId: string, signal?: AbortSignal): Promise<CloudWrite> {
		return this.json({
			method: "GET",
			path: `/agent-cli/writes/${encodeURIComponent(writeId)}`,
			timeoutMs: DEFAULT_TIMEOUT_MS,
			signal,
		});
	}

	decideWrite(
		writeId: string,
		body: { decision: "approve" | "decline"; reason?: string; trust?: boolean },
		signal?: AbortSignal,
	): Promise<CloudWrite> {
		return this.json({
			method: "POST",
			path: `/agent-cli/writes/${encodeURIComponent(writeId)}/decision`,
			body,
			timeoutMs: DEFAULT_TIMEOUT_MS,
			signal,
		});
	}

	runWrite(
		writeId: string,
		timeoutSeconds: number,
		signal?: AbortSignal,
	): Promise<WriteOutcome> {
		return this.json({
			method: "POST",
			path: `/agent-cli/writes/${encodeURIComponent(writeId)}/run`,
			timeoutMs: timeoutSeconds * 1000 + 30_000,
			signal,
		});
	}

	readCredits(conversationId: string): Promise<SessionCredits> {
		return this.json({
			method: "GET",
			path: `/agent-cli/sessions/${conversationId}/credits`,
			timeoutMs: DEFAULT_TIMEOUT_MS,
		});
	}

	async listModels(): Promise<GatewayModel[]> {
		const page = await this.json<{ models: GatewayModel[] }>({
			method: "GET",
			path: "/agent-cli/models",
			timeoutMs: 15_000,
		});
		return page.models;
	}

	whoami(): Promise<Identity> {
		return this.json({
			method: "GET",
			path: "/cli/whoami",
			timeoutMs: 15_000,
		});
	}

	getConnectionsContext(): Promise<ConnectionsContext> {
		return this.json({
			method: "GET",
			path: "/agent-cli/connections",
			timeoutMs: 15_000,
		});
	}

	setWorkspaceAutoMode(workspaceId: string, enabled: boolean): Promise<WorkspaceAutoMode> {
		return this.json({
			method: "PATCH",
			path: `/workspaces/${encodeURIComponent(workspaceId)}/auto-mode`,
			body: { enabled },
			timeoutMs: 15_000,
		});
	}

	async resendInterruptNotification(conversationId: string): Promise<void> {
		await this.send({
			method: "POST",
			path: "/notifications/interrupt/resend",
			query: { conversation_id: conversationId },
			timeoutMs: 15_000,
		});
	}

	listSkills(): Promise<WorkspaceSkill[]> {
		return this.json({
			method: "GET",
			path: "/custom-skills/",
			timeoutMs: 30_000,
		});
	}

	async downloadSkill(name: string): Promise<Uint8Array> {
		const response = await this.send({
			method: "GET",
			path: `/custom-skills/${encodeURIComponent(name)}/download`,
			timeoutMs: 60_000,
		});
		return new Uint8Array(await response.arrayBuffer());
	}

	submitRun(
		body: { prompt: string; conversation_id?: string; source_conversation_id?: string },
		signal?: AbortSignal,
	): Promise<RunSubmitted> {
		return this.json({
			method: "POST",
			path: "/cli/runs",
			body,
			timeoutMs: 60_000,
			signal,
		});
	}

	getRun(runId: string, signal?: AbortSignal): Promise<RunState> {
		return this.json({
			method: "GET",
			path: `/cli/runs/${encodeURIComponent(runId)}`,
			timeoutMs: DEFAULT_TIMEOUT_MS,
			signal,
		});
	}

	private async json<T>(options: CallOptions): Promise<T> {
		const response = await this.send(options);
		return (await response.json()) as T;
	}

	private async send(options: CallOptions): Promise<Response> {
		const response = await this.attempt(options);
		if (response.status === 401 && !this.tokens.fromEnvironment) {
			this.tokens.invalidate();
			return this.checked(await this.attempt(options));
		}
		return this.checked(response);
	}

	private async attempt(options: CallOptions): Promise<Response> {
		const url = new URL(`${this.apiUrl}${options.path}`);
		for (const [key, value] of Object.entries(options.query ?? {})) {
			if (value !== undefined) url.searchParams.set(key, String(value));
		}
		const headers: Record<string, string> = {
			Authorization: `Bearer ${this.tokens.resolve()}`,
			Accept: "application/json",
		};
		if (options.body !== undefined) headers["Content-Type"] = "application/json";
		const timeout = AbortSignal.timeout(options.timeoutMs);
		try {
			return await this.fetchImpl(url, {
				method: options.method,
				headers,
				body: options.body === undefined ? undefined : JSON.stringify(options.body),
				signal: options.signal ? AbortSignal.any([timeout, options.signal]) : timeout,
			});
		} catch (error) {
			throw new CloudThinkerApiError(0, describeTransportFailure(error, url));
		}
	}

	private async checked(response: Response): Promise<Response> {
		if (response.ok) return response;
		throw new CloudThinkerApiError(response.status, await readErrorMessage(response));
	}
}

function describeTransportFailure(error: unknown, url: URL): string {
	const reason = error instanceof Error ? error.message : String(error);
	return `Could not reach ${url.origin}: ${reason}`;
}

async function readErrorMessage(response: Response): Promise<string> {
	const fallback = `${response.status} ${response.statusText}`.trim();
	let body: unknown;
	try {
		body = JSON.parse(await response.text());
	} catch {
		return fallback;
	}
	if (typeof body !== "object" || body === null) return fallback;
	const envelope = body as { error?: { message?: unknown }; detail?: unknown };
	const message = envelope.error?.message;
	if (typeof message === "string" && message.length > 0) return message;
	const detail = envelope.detail;
	if (typeof detail === "string" && detail.length > 0) return detail;
	if (detail !== undefined && detail !== null) return JSON.stringify(detail);
	return fallback;
}
