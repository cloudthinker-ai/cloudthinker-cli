import { join } from "node:path";

import {
	SettingsManager,
	getAgentDir,
	getPackageDir,
	main,
} from "@earendil-works/pi-coding-agent";

import cloudthinker from "@cloudthinker/pi/src/index.ts";
import { markStartup } from "@cloudthinker/pi/src/timing.ts";

import { NO_SESSION_REFUSAL, applyGuard, hasNoSessionFlag } from "./guard.ts";
import { modelScopeArgs } from "./models.ts";
import bundledSubagents from "./subagents.ts";
import { bundledThemePaths, themeArgs } from "./theme.ts";

markStartup("agent.modules");
process.title = "cloudthinker";
process.env.PI_CODING_AGENT = "true";
process.env.AI_AGENT = "pi";
process.env.PI_SKIP_VERSION_CHECK = "1";

const argv = process.argv.slice(2);
if (hasNoSessionFlag(argv)) {
	process.stderr.write(`${NO_SESSION_REFUSAL}\n`);
	process.exit(2);
}

applyGuard();
markStartup("agent.guard");

const themes = themeArgs(
	bundledThemePaths(join(getPackageDir(), "theme")),
	argv,
	SettingsManager.create(process.cwd(), getAgentDir()).getThemeSetting(),
);
markStartup("agent.settings");

await main([...themes, ...modelScopeArgs(argv), ...argv], {
	extensionFactories: [
		{ name: "cloudthinker", factory: cloudthinker },
		{ name: "subagents", factory: bundledSubagents },
	],
});
