import assert from "node:assert/strict";
import { chmodSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import type { ExtensionAPI, ExtensionCommandContext } from "@earendil-works/pi-coding-agent";

import { execCommand } from "../node_modules/@earendil-works/pi-coding-agent/dist/core/exec.js";
import { registerDoctor } from "../src/doctor.ts";
import { bundledDoctorPath } from "../src/onboarding.ts";

interface Run {
	notices: { message: string; level: string }[];
	sent: { content: string; options: unknown }[];
}

const PROBLEMS = [
	" doctor · 0.13.1 · app.cloudthinker.io",
	" ✓ Install · Sign-in · Network · 4 of 6 Connections",
	" ✗ gcp-billing  GCP test failed at authenticate (HTTP 403)",
	" ? datadog      no answer before the deadline",
	" ! Release      0.13.2 is available · cloudthinker update",
].join("\n");

const SIGNED_OUT = [
	" doctor · 0.13.1 · app.cloudthinker.io",
	" ✓ Install · Network",
	" ✗ Login  not signed in · cloudthinker login · account not checked",
].join("\n");

const HEALTHY = " ✓ doctor · 0.13.1 · all checks passed";

async function runDoctor(launcher: string | undefined, output?: { stdout: string; exit: number }, idle = true): Promise<Run> {
	const root = mkdtempSync(join(tmpdir(), "ct-doctor-"));
	const previous = process.env.CLOUDTHINKER_BIN;
	const run: Run = { notices: [], sent: [] };
	try {
		let binary = launcher;
		if (output) {
			writeFileSync(join(root, "report.txt"), `${output.stdout}\n`);
			binary = join(root, "cloudthinker");
			writeFileSync(binary, `#!/bin/sh\n[ "$*" = "doctor" ] || exit 64\ncat "${join(root, "report.txt")}"\nexit ${output.exit}\n`);
			chmodSync(binary, 0o755);
		}
		if (binary === undefined) delete process.env.CLOUDTHINKER_BIN;
		else process.env.CLOUDTHINKER_BIN = binary;
		let handler: ((args: string, ctx: ExtensionCommandContext) => Promise<void>) | undefined;
		const pi = {
			registerCommand: (name: string, command: { handler: typeof handler }) => { if (name === "doctor") handler = command.handler; },
			exec: (command: string, args: string[], options?: { timeout?: number }) => execCommand(command, args, root, options),
			sendUserMessage: (content: string, options: unknown) => run.sent.push({ content, options }),
		} as unknown as ExtensionAPI;
		registerDoctor(pi);
		const ctx = {
			isIdle: () => idle,
			ui: { notify: (message: string, level: string) => run.notices.push({ message, level }), setStatus: () => {} },
		} as unknown as ExtensionCommandContext;
		await handler!("", ctx);
		return run;
	} finally {
		if (previous === undefined) delete process.env.CLOUDTHINKER_BIN;
		else process.env.CLOUDTHINKER_BIN = previous;
		rmSync(root, { recursive: true, force: true });
	}
}

test("DOCTOR-1: problems are shown verbatim and start one agent turn with the doctor skill and the text", async () => {
	const run = await runDoctor(undefined, { stdout: PROBLEMS, exit: 1 });
	assert.deepEqual(run.notices, [{ message: PROBLEMS, level: "info" }]);
	assert.equal(run.sent.length, 1);
	assert.equal(run.sent[0]!.content, `/skill:doctor /doctor ran \`cloudthinker doctor\` and printed:\n\n\`\`\`text\n${PROBLEMS}\n\`\`\``);
	assert.deepEqual(run.sent[0]!.options, { expandPromptTemplates: true });
});

test("DOCTOR-2: signed out is shown verbatim and starts the agent turn", async () => {
	const run = await runDoctor(undefined, { stdout: SIGNED_OUT, exit: 3 });
	assert.equal(run.notices[0]!.message, SIGNED_OUT);
	assert.equal(run.sent.length, 1);
});

test("DOCTOR-3: the all-passed line starts no agent turn", async () => {
	const run = await runDoctor(undefined, { stdout: HEALTHY, exit: 0 });
	assert.deepEqual(run.notices, [{ message: HEALTHY, level: "info" }]);
	assert.equal(run.sent.length, 0);
});

test("DOCTOR-4: the all-passed line with a non-zero exit still asks the agent", async () => {
	const run = await runDoctor(undefined, { stdout: HEALTHY, exit: 1 });
	assert.equal(run.sent.length, 1);
});

test("DOCTOR-5: a missing launcher says so and starts no agent turn", async () => {
	const run = await runDoctor("/nonexistent/ct-doctor/cloudthinker");
	assert.equal(run.notices.length, 1);
	assert.equal(run.notices[0]!.level, "error");
	assert.match(run.notices[0]!.message, /^\/doctor could not run `\/nonexistent\/ct-doctor\/cloudthinker doctor` \(exit \d+\)/);
	assert.match(run.notices[0]!.message, /cloudthinker update/);
	assert.equal(run.sent.length, 0);
});

test("DOCTOR-6: a busy agent gets the doctor turn as a follow-up", async () => {
	const run = await runDoctor(undefined, { stdout: PROBLEMS, exit: 1 }, false);
	assert.deepEqual(run.sent[0]!.options, { expandPromptTemplates: true, deliverAs: "followUp" });
});

test("DOCTOR-7: the doctor skill ships beside the tour skill", () => {
	assert.match(bundledDoctorPath("/nonexistent"), /agent\/\.agents\/skills\/doctor\/SKILL\.md$/);
});
