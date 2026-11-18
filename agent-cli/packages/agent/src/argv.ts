export function takeFlagValues(argv: readonly string[], flag: string): { values: string[]; rest: string[] } {
	const separator = argv.indexOf("--");
	const end = separator === -1 ? argv.length : separator;
	const values: string[] = [];
	const rest: string[] = [];
	for (let index = 0; index < argv.length; index += 1) {
		const arg = argv[index] as string;
		if (index < end && arg === flag) {
			const value = argv[index + 1];
			if (value !== undefined) values.push(value);
			index += 1;
		} else {
			rest.push(arg);
		}
	}
	return { values, rest };
}
