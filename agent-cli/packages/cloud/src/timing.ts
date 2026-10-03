export function createPhaseTimer(
	enabled: boolean,
	clock: () => number = () => performance.now(),
	write: (line: string) => void = (line) => { process.stderr.write(line); },
): (phase: string) => void {
	if (!enabled) return () => {};
	let previous = 0;
	return (phase) => {
		const elapsed = clock();
		write(`[cloudthinker timing] ${JSON.stringify({ phase, elapsed_ms: elapsed, phase_ms: elapsed - previous })}\n`);
		previous = elapsed;
	};
}

export const markStartup = createPhaseTimer(process.env.CLOUDTHINKER_TIMING === "1");
