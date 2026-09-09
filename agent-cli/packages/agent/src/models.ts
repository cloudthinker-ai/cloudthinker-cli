import { PROVIDER_ID } from "@cloudthinker/pi/src/provider.ts";

export const MODEL_SCOPE = `${PROVIDER_ID}/*`;

export function modelScopeArgs(argv: string[]): string[] {
	return argv.includes("--models") ? [] : ["--models", MODEL_SCOPE];
}
