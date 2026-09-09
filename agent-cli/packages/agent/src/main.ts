import { join } from "node:path";

import {
	SettingsManager,
	getAgentDir,
	getPackageDir,
	main,
} from "@earendil-works/pi-coding-agent";

import cloudthinker from "@cloudthinker/pi/src/index.ts";

import { NO_SESSION_REFUSAL, applyGuard, hasNoSessionFlag } from "./guard.ts";
import { modelScopeArgs } from "./models.ts";
import { bundledThemePaths, themeArgs } from "./theme.ts";
import { tuiModeArgs } from "./tui.ts";

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

const themes = themeArgs(
	bundledThemePaths(join(getPackageDir(), "theme")),
	argv,
	SettingsManager.create(process.cwd(), getAgentDir()).getThemeSetting(),
);

await main([...themes, ...tuiModeArgs(argv), ...modelScopeArgs(argv), ...argv], {
	extensionFactories: [{ name: "cloudthinker", factory: cloudthinker }],
});
