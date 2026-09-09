import { basename } from "node:path";

import { SettingsManager, getAgentDir } from "@earendil-works/pi-coding-agent";
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";

import { withinBudget } from "./async.ts";
import { registerCommands } from "./commands.ts";
import { CreditsMeter } from "./credits.ts";
import { PRODUCT_NAME } from "./header.ts";
import { LocationTracker } from "./location.ts";
import { fetchMemory } from "./memory.ts";
import { SessionMirror, formatMirrorStatus } from "./mirror.ts";
import { appendPromptBlock, buildPromptBlock } from "./prompt.ts";
import {
	MODELS_UNAVAILABLE_STATUS,
	applyConversationHeader,
	modelsUnavailableMessage,
	pinProviderWorkspace,
	registerProvider,
} from "./provider.ts";
import { CloudThinkerRuntime, describeError, detach } from "./runtime.ts";
import { refreshConnections, startSession } from "./session.ts";
import { discoverSkillPaths, hasSkillIndex, refreshSkills } from "./skills.ts";
import { registerAsk } from "./tools/ct-ask.ts";
import { registerCloudRead } from "./tools/ct-cloud-read.ts";
import { registerCloudWrite } from "./tools/ct-cloud-write.ts";
import { registerRunStatus } from "./tools/ct-run-status.ts";
import { registerReadTaskOutput } from "./tools/read-task-output.ts";

const SHUTDOWN_FLUSH_MS = 5_000;

export function sessionTitle(cwd: string, workspaceName: string | undefined): string {
	const parts = [PRODUCT_NAME, basename(cwd)];
	if (workspaceName) parts.push(workspaceName);
	return parts.join(" · ");
}

export default async function cloudthinker(pi: ExtensionAPI): Promise<void> {
	const runtime = new CloudThinkerRuntime(pi);
	const mirror = new SessionMirror(runtime.client, (status) =>
		runtime.setStatus(formatMirrorStatus(status)),
	);
	const locations = new LocationTracker(pi);
	const credits = new CreditsMeter(runtime);

	const warn = (label: string) => (error: unknown) => {
		runtime.notify(`CloudThinker ${label}: ${describeError(error)}`, "warning");
	};
	const silent = (): void => {};

	const modelsUnavailable = await registerProvider(runtime);
	let modelsUnavailableAnnounced = false;
	registerCloudRead(runtime);
	registerCloudWrite(runtime);
	registerReadTaskOutput(runtime);
	registerAsk(runtime);
	registerRunStatus(runtime);
	registerCommands(runtime);

	const workspaceId = (): string | undefined =>
		runtime.identity?.workspace_id ?? runtime.session?.workspace_id;

	const sync = (ctx: ExtensionContext): void => {
		runtime.bind(ctx);
		mirror.sync(ctx.sessionManager.getEntries(), silent);
	};

	pi.on("session_start", async (event, ctx) => {
		runtime.bind(ctx);
		mirror.unlink();
		runtime.reset();
		locations.reset();
		if (ctx.mode === "tui") {
			runtime.setTitle(sessionTitle(ctx.cwd, undefined));
			if (!SettingsManager.create(ctx.cwd, getAgentDir()).getQuietStartup()) {
				ctx.ui.setHeader(runtime.header.factory);
			}
		}
		if (modelsUnavailable) {
			runtime.setStatus(MODELS_UNAVAILABLE_STATUS);
			if (!modelsUnavailableAnnounced) {
				modelsUnavailableAnnounced = true;
				runtime.notify(modelsUnavailableMessage(modelsUnavailable), "error");
			}
		}
		await startSession(runtime, event, ctx);
		if (ctx.mode === "tui") {
			runtime.setTitle(sessionTitle(ctx.cwd, runtime.identity?.workspace_name));
		}
		const session = runtime.session;
		if (!session) return;
		pinProviderWorkspace(runtime, session.workspace_id);
		detach(async () => {
			await mirror.link(session.conversation_id).catch(silent);
			sync(ctx);
			credits.refresh();
		}, silent);
		detach(() => locations.record(ctx.cwd), silent);
		detach(async () => {
			runtime.memory = await fetchMemory(runtime.client, session.conversation_id);
		}, silent);
		const skillsWorkspace = workspaceId();
		if (skillsWorkspace) {
			const refresh = async (): Promise<void> => {
				await refreshSkills(runtime.client, skillsWorkspace, undefined, (name, error) => {
					warn(`skill "${name}"`)(error);
				});
			};
			if (await hasSkillIndex(skillsWorkspace)) {
				detach(refresh, warn("skills"));
			} else {
				await refresh().catch(warn("skills"));
			}
		}
	});

	pi.on("resources_discover", async () => ({
		skillPaths: await discoverSkillPaths(workspaceId()),
	}));

	pi.on("before_provider_headers", (event, ctx) => {
		runtime.bind(ctx);
		applyConversationHeader(
			event.headers,
			ctx.model?.provider,
			runtime.session?.conversation_id,
		);
	});

	pi.on("before_agent_start", (event, ctx) => {
		runtime.bind(ctx);
		detach(() => refreshConnections(runtime), silent);
		return {
			systemPrompt: appendPromptBlock(event.systemPrompt, buildPromptBlock(runtime)),
		};
	});

	pi.on("turn_end", (_event, ctx) => {
		sync(ctx);
		detach(() => locations.record(ctx.cwd), silent);
	});
	pi.on("agent_end", (_event, ctx) => {
		sync(ctx);
		credits.refresh();
	});
	pi.on("session_compact", (_event, ctx) => sync(ctx));
	pi.on("session_tree", (_event, ctx) => sync(ctx));

	pi.on("session_shutdown", async (_event, ctx) => {
		runtime.bind(ctx);
		await withinBudget(
			mirror.flush(ctx.sessionManager.getEntries()).catch(silent),
			SHUTDOWN_FLUSH_MS,
		);
	});
}
