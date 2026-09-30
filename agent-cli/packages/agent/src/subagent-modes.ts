type AvailableMode = {
	provider: string;
	id: string;
};

const TIERS = [
	["light", "bounded lookups, execution, or bookkeeping"],
	["pro", "open-ended analysis or implementation"],
	["ultra", "unusually difficult reasoning or high-consequence verification, only when warranted"],
] as const;

export function adaptiveSubagentModeGuidance(modes: readonly AvailableMode[]): string {
	const advertised = modes.map((mode) => `${mode.provider}/${mode.id}`);
	const selectors = new Map(modes.map((mode) => [mode.id.toLowerCase(), `${mode.provider}/${mode.id}`]));
	const tierGuidance = TIERS.flatMap(([tier, task]) => {
		const selector = selectors.get(tier);
		return selector ? [`${selector} for ${task}`] : [];
	});
	const tiers = tierGuidance.join(", ") || "no tier mode is available";
	return `For each workflow worker, set model to an advertised fit: ${tiers}. Honor explicit model choices; omit model only when inheriting the parent is intentional. Advertised modes: ${advertised.join(", ") || "none available"}.`;
}
