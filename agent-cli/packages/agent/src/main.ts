import { existsSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import {
	SettingsManager,
	getAgentDir,
	getPackageDir,
	main,
} from "@earendil-works/pi-coding-agent";

import cloudthinker from "@cloudthinker/cloud/src/index.ts";
import { markStartup } from "@cloudthinker/cloud/src/timing.ts";

import { registerBackgroundCommands } from "./background/index.ts";
import { registerMentionPicker } from "./mention-picker.ts";
import bundledBtw from "./btw.ts";
import { applyGuard } from "./guard.ts";
import { isLocalReview, withoutLocalReviewFlag } from "./local-review-mode.ts";
import { modelScopeArgs } from "./models.ts";
import { registerScrollPill } from "./scroll-pill.ts";
import { AGENT_HELP, checkSurface } from "./surface.ts";
import bundledSubagents from "./subagents.ts";
import { bundledThemePaths, themeArgs } from "./theme.ts";
import { savedTuiMode, tuiModeArgs } from "./tui-mode.ts";
import { registerVerbosity } from "./verbosity.ts";

markStartup("agent.modules");
process.title = "cloudthinker";
process.env.PI_CODING_AGENT = "true";
process.env.AI_AGENT = "pi";
process.env.PI_SKIP_VERSION_CHECK = "1";
process.env.PI_TELEMETRY = "0";

const rawArgs = process.argv.slice(2);
const localReview = isLocalReview(rawArgs);
const argv = withoutLocalReviewFlag(rawArgs);
const surface = checkSurface(argv);
if (surface.kind === "help") {
	process.stdout.write(AGENT_HELP);
	process.exit(0);
}
if (surface.kind === "refuse") {
	process.stderr.write(`${surface.message}\n`);
	process.exit(2);
}

applyGuard();
markStartup("agent.guard");

const sourceChangelog = fileURLToPath(new URL("../CHANGELOG.md", import.meta.url));
const changelogPath = existsSync(sourceChangelog) ? sourceChangelog : join(getPackageDir(), "CHANGELOG.md");

const settings = SettingsManager.create(process.cwd(), getAgentDir());
const themes = themeArgs(
	bundledThemePaths(join(getPackageDir(), "theme")),
	argv,
	settings.getThemeSetting(),
);
const tuiMode = tuiModeArgs(argv, savedTuiMode(settings));
markStartup("agent.settings");

await main([...themes, ...tuiMode, ...modelScopeArgs(argv), ...argv], {
	extensionFactories: [
		{ name: "cloudthinker", factory: (pi) => cloudthinker(pi, { localReview, changelogPath }) },
		{ name: "background", factory: (pi) => { registerBackgroundCommands(pi); } },
		{ name: "verbosity", factory: (pi) => { registerVerbosity(pi); } },
		{ name: "scroll-pill", factory: (pi) => { registerScrollPill(pi); } },
		...(localReview ? [] : [
			{ name: "subagents", factory: bundledSubagents },
			{ name: "btw", factory: bundledBtw },
		]),
		{ name: "mentions", factory: (pi) => { registerMentionPicker(pi); } },
	],
});
