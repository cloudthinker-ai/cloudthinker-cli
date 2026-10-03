import { createAgentSession } from "@earendil-works/pi-coding-agent";
import type { CreateAgentSessionOptions, CreateAgentSessionResult, DefaultResourceLoader, ExtensionContext } from "@earendil-works/pi-coding-agent";
import type { ModelRegistry } from "./model-resolver.ts";

type DefaultResourceLoaderOptions = ConstructorParameters<typeof DefaultResourceLoader>[0];

export interface SubagentHost {
  modelChoices(ctx: ExtensionContext): string[];
  resolveModel(input: string, registry: Pick<ModelRegistry, "find" | "getAvailable"> & Partial<Pick<ModelRegistry, "getAll">>): NonNullable<CreateAgentSessionOptions["model"]> | string;
  loaderOptions(options: DefaultResourceLoaderOptions, ctx: ExtensionContext, includeCloudTools: boolean): DefaultResourceLoaderOptions;
  createSession(options: CreateAgentSessionOptions, ctx: ExtensionContext): Promise<CreateAgentSessionResult>;
  taskRows?(rows: ((tui: any, theme: any) => string[]) | undefined, active: number): void;
  tasks?(): HostTask[];
  openTask?(id: string, ui: any): Promise<void>;
  stopTask?(id: string): Promise<void>;
}
export interface HostTask { id: string; label: string; stats: string }
let host: SubagentHost | undefined;
export function setSubagentHost(value: SubagentHost): void { host = value; }
export function getSubagentHost(): SubagentHost | undefined { return host; }

export function createSubagentSession(options: CreateAgentSessionOptions, ctx: ExtensionContext): Promise<CreateAgentSessionResult> {
  return host?.createSession(options, ctx) ?? createAgentSession(options);
}
