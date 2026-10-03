import { getSubagentHost, createSubagentSession } from "./host.ts";
/**
 * mention-clone.ts — start a mentioned agent through a clone of this
 * conversation, without putting anything in the chat.
 *
 * Claude Code routes `@agent-<type>` through the main model: the mention
 * becomes a `<system-reminder>` appended to the prompt and the model makes the
 * tool call (see `agentMentionReminder`). That buys the spawned agent a prompt
 * written with conversation context, and costs a visible turn — the model's
 * reasoning and its tool block land in the transcript, for a decision the user
 * already made when they typed the handle.
 *
 * So the turn happens somewhere else. The conversation is cloned into a
 * throwaway in-memory session — same messages, same system prompt, same model —
 * and that copy takes the turn off-screen. A literal clone: the session's own
 * entries, projected by pi's own `sessionEntryToContextMessages`, not
 * `inherit_context`'s text rendering of them.
 *
 * Cloned from memory rather than from the session file, which cannot be relied
 * on: `SessionManager._persist` withholds every write until the first assistant
 * message lands, so a fork taken before then reads an empty file and throws.
 * The in-memory leaf path has no such timing, and pi projects it compaction-aware
 * — the summary replaces entries folded into it, so a long conversation clones as
 * what the main model is actually working from. Pi 1.0 made the SessionManager
 * canonical for provider context, so the entries seed the clone's manager rather
 * than its agent state. A conversation with nothing in it yet clones to nothing
 * in it yet, which is the correct answer rather than a failure.
 *
 * Its `thinkingLevel` is NOT used, and is the one place the newer API would be
 * better. `getSessionContextSettings` starts at "off" and moves only on an
 * explicit `thinking_level_change` entry, so a session where nobody ran
 * `/think` reports "off" rather than the level it is really using. Omitting the
 * field instead lets `createAgentSession` resolve it from settings, which is
 * that real level.
 *
 * Three details make the spawn belong to the real session rather than the
 * clone:
 *
 *   - the clone is handed the *registered* `Agent` tool, whose handler closes
 *     over the main activation, so it spawns top-level: widget, fleet row,
 *     handle, completion notification, all as if the main model had called it;
 *   - that tool is re-bound to the main `ExtensionContext`, because the handler
 *     reads `cwd`, `model` and `sessionManager.getSessionId()` off it to place
 *     the transcript and the `rootSessionId`. The clone's own context would
 *     file both under the throwaway fork;
 *   - it is called with no tool-call id. The clone's turn produces one, but the
 *     real session never issued it, and a `<tool-use-id>` pointing at nothing
 *     is exactly the bug the mention-resume path had to fix;
 *   - and it is forced into the background. A foreground agent returns its
 *     answer as the tool result and is marked `resultConsumed` so no completion
 *     notification is sent — correct when the caller is the real conversation,
 *     silent loss when the caller is a fork about to be discarded. Background
 *     delivery is the only route from a mention back to the main model.
 *
 * The clone gets one tool and one job. It cannot read, write or run anything —
 * an invisible turn with the full toolset could do invisible work.
 */

import { getCurrentSystemMessage, type Model } from "@earendil-works/pi-ai";
import {
  createAgentSession,
  type ExtensionContext,
  SessionManager,
  type ToolDefinition,
} from "@earendil-works/pi-coding-agent";
import { runInChildSessionContext } from "./child-context.ts";
import { agentMentionReminder } from "./mention.ts";
import type { SubagentType, ThinkingLevel } from "./types.ts";

export interface MentionCloneOptions {
  /** The MAIN session's context — what the spawn is attributed to, and the
   * source of both the conversation and the live system prompt. */
  ctx: ExtensionContext;
  /** Agent type the handle resolved to. */
  type: SubagentType;
  /** What the user typed after the handle. */
  message: string;
  /** The registered `Agent` tool, reused so the spawn is an ordinary one. */
  agentTool: ToolDefinition;
}

export interface MentionCloneResult {
  /** True once the clone actually called `Agent`. */
  spawned: boolean;
  /** Why not, when it didn't. Absent on success. */
  error?: string;
}

/**
 * Fork the conversation, let the copy make the tool call, throw the copy away.
 * Never rejects: a clone that cannot run is reported so the caller can fall
 * back to starting the agent directly.
 */
export async function runMentionClone(opts: MentionCloneOptions): Promise<MentionCloneResult> {
  const { ctx, type, message, agentTool } = opts;

  let spawned = false;
  const cloneAgentTool: ToolDefinition = {
    ...agentTool,
    execute: (_cloneToolCallId, params, signal, onUpdate, cloneCtx) => {
      // One spawn per mention. The clone has a single tool and every reason to
      // stop after using it, but a model that decides to "also" launch a second
      // agent would do it where nobody can see and nobody asked.
      if (spawned) {
        return Promise.resolve({
          content: [{ type: "text" as const, text: "Already started an agent for this mention. Stop here." }],
          details: undefined,
          isError: true,
        });
      }
      spawned = true;
      // undefined tool-call id + the main ctx: see the header. Background is
      // forced rather than left to the clone: `run_in_background` defaults to
      // false, and a foreground agent answers through its TOOL RESULT — which
      // here is delivered into a session that is disposed moments later, so the
      // agent would run, appear in the widget and the fleet, and reach nobody.
      return agentTool.execute(
        undefined as never,
        { ...(params as Record<string, unknown>), run_in_background: true } as typeof params,
        signal,
        onUpdate,
        Object.assign(Object.create(ctx) as ExtensionContext, { tools: cloneCtx.tools, executeTool: cloneCtx.executeTool }),
      );
    },
  };

  let session: Awaited<ReturnType<typeof createAgentSession>>["session"] | undefined;
  try {
    // Pi 0.80.8 moved createAgentSession from modelRegistry to modelRuntime;
    // agent-runner.ts carries the same shim for the same reason — pass both so
    // the clone keeps the parent's providers across the supported range.
    const parentModelRuntime = (ctx.modelRegistry as unknown as { runtime?: unknown }).runtime;
    // The conversation as the main session resolves it: compaction applied,
    // branch summaries substituted.
    const header = ctx.sessionManager.getHeader();
    const conversation = [...(header ? [header] : []), ...ctx.sessionManager.getBranch()];
    // Pi 0.82.0 added this; below it the field is absent and the clone takes
    // the settings level instead, which is what a session that never ran
    // `/think` is on anyway. Same shim shape as `modelRuntime` below.
    const thinkingLevel = (ctx as { thinkingLevel?: ThinkingLevel }).thinkingLevel;
    const created = await runInChildSessionContext(() =>
      createSubagentSession({
        cwd: ctx.cwd,
        // Nothing about the copy is worth persisting, and an in-memory manager
        // is also what keeps the real session untouched.
        sessionManager: SessionManager.inMemory(ctx.cwd, undefined, conversation),
        model: ctx.model as Model<never> | undefined,
        ...(thinkingLevel && { thinkingLevel }),
        modelRegistry: ctx.modelRegistry,
        ...(parentModelRuntime !== undefined && { modelRuntime: parentModelRuntime as never }),
        // An allowlist naming exactly the clone's own tool. NOT `noTools:
        // "all"`, whose doc comment ("start with no tools enabled") reads like
        // it spares custom tools and does not: it resolves to an EMPTY
        // allowlist, and `isAllowedTool` then drops every tool from the
        // registry — the custom one included. The clone would be prompted with
        // nothing to call, answer in prose, and every mention would fall
        // through to the direct start with a warning. Same idiom as
        // agent-runner's `tools: sessionTools` beside its nested `customTools`.
        tools: [cloneAgentTool.name],
        customTools: [cloneAgentTool],
      } as NonNullable<Parameters<typeof createAgentSession>[0]>, ctx),
    );
    session = created.session;

    // The clone rebuilds a system prompt from cwd and agentDir, which is close
    // but not the live one — extensions contribute to it per turn. Copy the
    // real thing, so the copy reasons under the instructions the user's model
    // is actually working under.
    const systemPrompt = ctx.getSystemPrompt?.();
    if (systemPrompt) {
      const transform = session.agent.transformContext;
      session.agent.transformContext = async (messages, signal) => {
        const transformed = transform ? await transform(messages, signal) : messages;
        const current = getCurrentSystemMessage(transformed);
        return [
          { role: "system", content: systemPrompt, ...(current?.toolsAdded && { toolsAdded: current.toolsAdded }), timestamp: current?.timestamp ?? Date.now() },
          ...transformed.filter((message) => message.role !== "system"),
        ];
      };
    }

    // User text first, reminder after — the order Claude Code's attachment
    // renderer produces, where the reminder trails the message it is about.
    await session.prompt(`${message}\n\n${agentMentionReminder(type)}`);
  } catch (err) {
    return { spawned, error: err instanceof Error ? err.message : String(err) };
  } finally {
    if (getSubagentHost() && session) await session.extensionRunner.emit({ type: "session_shutdown", reason: "quit" });
    session?.dispose?.();
  }

  return spawned
    ? { spawned: true }
    : { spawned: false, error: "the conversation clone did not start it" };
}
