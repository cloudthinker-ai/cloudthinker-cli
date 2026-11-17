import { join } from "node:path";
import { getAgentDir } from "@earendil-works/pi-coding-agent";

export const projectRoot = (cwd: string, folder: string) => join(cwd, ".pi", folder);
export const workspaceRoot = (cwd: string, folder: string) => join(cwd, ".agents", folder);
export const userRoot = (folder: string) => join(getAgentDir(), folder);
