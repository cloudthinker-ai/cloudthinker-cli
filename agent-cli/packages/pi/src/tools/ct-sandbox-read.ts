import { type Static, Type } from "typebox";

import type { ExecutionResult } from "../client.ts";
import { MEMORY_DIR } from "../memory.ts";
import type { CloudThinkerRuntime } from "../runtime.ts";
import { CT_SANDBOX_READ, CT_SANDBOX_WRITE, READ_TASK_OUTPUT } from "./names.ts";
import {
	type Elapsed,
	callComponent,
	callLine,
	firstLine,
	formatElapsed,
	resultBody,
	summaryComponent,
} from "./render.ts";
import { explain, section, text } from "./shared.ts";

export const MAX_TIMEOUT_SECONDS = 120;
export const DEFAULT_TIMEOUT_SECONDS = 60;
export const SANDBOX_WORKING_MESSAGE = "Running in CloudThinker Sandbox…";

const parameters = Type.Object({
	connection_list: Type.Array(Type.String(), {
		description:
			"Connection prefixes whose credentials the script needs, drawn ONLY " +
			"from the connected prefixes listed in the system prompt. Empty for a " +
			"command that needs no cloud credential.",
	}),
	script: Type.String({
		description:
			"A read-only shell command to run in the Sandbox. The Connection's " +
			"CLI is already installed and authenticated, and the ordinary shell " +
			"tools (ls, cat, grep, find) are there for the Sandbox's own files. " +
			"Bound the output yourself (head, --max-items, jq) so a large listing " +
			"does not fill the context.",
	}),
	timeout: Type.Optional(
		Type.Integer({
			minimum: 1,
			maximum: MAX_TIMEOUT_SECONDS,
			description: `Seconds to wait, at most ${MAX_TIMEOUT_SECONDS}. Defaults to ${DEFAULT_TIMEOUT_SECONDS}. A longer read belongs in run_in_background.`,
		}),
	),
	run_in_background: Type.Optional(
		Type.Boolean({
			description:
				"Detach the script and return a task_id in about a second. Poll " +
				`${READ_TASK_OUTPUT} with that id for the output.`,
		}),
	),
});

const description = [
	"Run a READ-ONLY command in the CloudThinker Sandbox, the workspace's own machine in the cloud, with a workspace Connection's credential injected when the command needs one.",
	"Use it for any fact that lives in the user's cloud: AWS, Kubernetes, GitHub, Datadog, and every other connected provider.",
	"",
	`The Sandbox is a durable machine the whole workspace shares, not a scratch shell. Its filesystem carries the workspace's memory tree at ${MEMORY_DIR}, its skills, and what earlier runs left behind, so it answers questions about the workspace itself and not only about a provider. Pass an empty connection_list for a command that reaches nothing but the Sandbox, such as reading one of those files.`,
	"",
	"The credential stays in the cloud. It is never sent to this machine and you never see it, you only get stdout back.",
	"Never ask the user for a production credential, access key, kubeconfig, or token. Call this tool instead.",
	"",
	"connection_list must name only prefixes from the connected list in the system prompt. Any other prefix is rejected.",
	"",
	"Read-only means read-only: describe, list, get, logs, query, and their equivalents.",
	`Anything that changes cloud state (create, delete, apply, scale, restart, rotate, put, tag) belongs in ${CT_SANDBOX_WRITE}, which the workspace approves.`,
].join("\n");

export function renderExecution(result: ExecutionResult): string {
	if (result.status === "running") {
		return [
			`Started in the background as task ${result.task_id}.`,
			`Call ${READ_TASK_OUTPUT} with task_id "${result.task_id}" to read its output.`,
		].join("\n");
	}
	return [
		`return_code: ${result.return_code}`,
		"",
		section("stdout", result.stdout),
		"",
		section("stderr", result.stderr),
	].join("\n");
}

export function registerSandboxRead(runtime: CloudThinkerRuntime): void {
	runtime.pi.registerTool<typeof parameters, ExecutionResult & Elapsed>({
		name: CT_SANDBOX_READ,
		label: "Sandbox read",
		description,
		promptSnippet:
			"Run a read-only command on the workspace's CloudThinker Sandbox, with a Connection's credential when the command needs one",
		promptGuidelines: [
			`Reach a workspace Connection only through ${CT_SANDBOX_READ}. This machine holds no cloud credentials, so a local aws, kubectl, gcloud, or gh command cannot reach the user's cloud.`,
			"Never ask the user to paste a cloud credential.",
		],
		parameters,
		execute: async (
			_toolCallId: string,
			params: Static<typeof parameters>,
			signal: AbortSignal | undefined,
			_onUpdate,
			ctx,
		) => {
			const session = runtime.requireSession();
			const startedAt = Date.now();
			if (ctx.hasUI) ctx.ui.setWorkingMessage(SANDBOX_WORKING_MESSAGE);
			try {
				const result = await runtime.client.execute(
					{
						conversation_id: session.conversation_id,
						connection_list: params.connection_list,
						script: params.script,
						timeout: params.timeout ?? DEFAULT_TIMEOUT_SECONDS,
						run_in_background: params.run_in_background ?? false,
					},
					signal,
				);
				return text(renderExecution(result), {
					...result,
					elapsed_ms: Date.now() - startedAt,
				});
			} catch (error) {
				throw new Error(explain(error, params.connection_list, runtime));
			} finally {
				if (ctx.hasUI) ctx.ui.setWorkingMessage();
			}
		},
		renderCall: (params: Static<typeof parameters>, theme) =>
			callComponent(
				callLine(
					theme,
					CT_SANDBOX_READ,
					params.connection_list?.join(", ") ?? "",
					firstLine(params.script ?? ""),
				),
			),
		renderResult: (result, options, theme) =>
			summaryComponent(
				theme,
				`ran in CloudThinker Sandbox · ${formatElapsed(result.details?.elapsed_ms)}`,
				resultBody(result),
				options.expanded,
			),
	});
}
