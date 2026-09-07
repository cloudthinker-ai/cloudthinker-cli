export function singleFlight<A = void>(work: (argument: A) => Promise<void>): (argument: A) => Promise<void> {
	let running = false;
	let rerun = false;
	let latest: A;
	return async (argument: A) => {
		latest = argument;
		if (running) {
			rerun = true;
			return;
		}
		running = true;
		try {
			do {
				rerun = false;
				await work(latest);
			} while (rerun);
		} finally {
			running = false;
		}
	};
}

export function withinBudget<T>(work: Promise<T>, ms: number): Promise<T | undefined> {
	return Promise.race([
		work,
		new Promise<undefined>((resolve) => setTimeout(() => resolve(undefined), ms).unref?.()),
	]);
}
