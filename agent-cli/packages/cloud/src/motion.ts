type Env = Readonly<Record<string, string | undefined>>;

export const LOCAL_TICK_MS = 80;
export const SERVER_TICK_MS = 400;

export function remoteSession(env: Env = process.env): boolean {
	return env.CLOUDTHINKER_REMOTE === "1" || [env.SSH_CONNECTION, env.SSH_CLIENT, env.SSH_TTY, env.MOSH_CONNECTION].some(Boolean);
}

export function reducedMotion(env: Env = process.env): boolean {
	if (env.CLOUDTHINKER_CLI_MOTION === "1") return false;
	return remoteSession(env);
}

export function tickMs(env: Env = process.env): number {
	return reducedMotion(env) ? SERVER_TICK_MS : LOCAL_TICK_MS;
}
