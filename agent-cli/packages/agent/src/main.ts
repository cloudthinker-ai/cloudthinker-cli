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
import { registerIdentity } from "./identity.ts";
import { isLocalReview, withoutLocalReviewFlag } from "./local-review-mode.ts";
import { modelScopeArgs } from "./models.ts";
import { registerNotifications } from "./notify.ts";
import { applyNotifySettingGuard } from "./notify-setting.ts";
import { extractAddDirs, registerAddDirs } from "./add-dir.ts";
import { extractMaxTurns, registerMaxTurns } from "./max-turns.ts";
import { registerRemoteDetach } from "./remote-leave.ts";
import { announceRemoteSession } from "./session-recovery.ts";
import { registerScrollPill } from "./scroll-pill.ts";
import { AGENT_HELP, checkSurface } from "./surface.ts";
import bundledSubagents from "./subagents.ts";
import { bundledThemePaths, themeArgs } from "./theme.ts";
import { savedTuiMode, tuiModeArgs } from "./tui-mode.ts";
import { registerRewind } from "./rewind.ts";
import { registerVerbosity } from "./verbosity.ts";

markStartup("agent.modules");
process.title = "cloudthinker";
process.env.PI_CODING_AGENT = "true";
process.env.AI_AGENT = "pi";
process.env.PI_SKIP_VERSION_CHECK = "1";
process.env.PI_TELEMETRY = "0";
delete process.env.BUN_JSC_numberOfGCMarkers;

const rawArgs = process.argv.slice(2);
const localReview = isLocalReview(rawArgs);
const surface = checkSurface(withoutLocalReviewFlag(rawArgs));
if (surface.kind === "help") {
	process.stdout.write(AGENT_HELP);
	process.exit(0);
}
if (surface.kind === "refuse") {
	process.stderr.write(`${surface.message}\n`);
	process.exit(2);
}

const addDirs = extractAddDirs(withoutLocalReviewFlag(rawArgs), process.cwd());
if (addDirs.error !== undefined) {
	process.stderr.write(`${addDirs.error}\n`);
	process.exit(2);
}
const turnLimit = extractMaxTurns(addDirs.rest);
const argv = turnLimit.rest;

applyGuard();
applyNotifySettingGuard();
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
		{ name: "identity", factory: (pi) => { registerIdentity(pi); } },
		{ name: "cloudthinker", factory: (pi) => cloudthinker(pi, { localReview, changelogPath }) },
		{ name: "background", factory: (pi) => { registerBackgroundCommands(pi); } },
		{ name: "verbosity", factory: (pi) => { registerVerbosity(pi); } },
		{ name: "rewind", factory: (pi) => { registerRewind(pi); } },
		{ name: "scroll-pill", factory: (pi) => { registerScrollPill(pi); } },
		{ name: "notify", factory: (pi) => { registerNotifications(pi); } },
		{ name: "add-dir", factory: (pi) => { registerAddDirs(pi, addDirs.dirs, getAgentDir()); } },
		{ name: "max-turns", factory: (pi) => { registerMaxTurns(pi, turnLimit.limit); } },
		{ name: "remote", factory: (pi) => {
			registerRemoteDetach(pi);
			if (process.env.CLOUDTHINKER_REMOTE === "1") {
				pi.on("session_start", (_event, ctx) => {
					if (ctx.mode === "tui") announceRemoteSession(ctx.sessionManager);
				});
			}
		} },
		...(localReview ? [] : [
			{ name: "subagents", factory: bundledSubagents },
			{ name: "btw", factory: bundledBtw },
		]),
		{ name: "mentions", factory: (pi) => { registerMentionPicker(pi); } },
	],
});
