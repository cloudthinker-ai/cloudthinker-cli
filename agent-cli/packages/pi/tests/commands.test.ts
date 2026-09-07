import assert from "node:assert/strict";
import test from "node:test";

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

import { type CloudThinkerClient, CloudThinkerApiError } from "../src/client.ts";
import {
	APPROVERS_NOTIFIED,
	AUTO_MODE_EDITOR_HINT,
	NOTHING_WAITING,
	aboutLines,
	autoCommand,
	autoModeLines,
	notifyApprovers,
} from "../src/commands.ts";
import { sessionTitle } from "../src/index.ts";
import { CloudThinkerRuntime } from "../src/runtime.ts";
import { PI_AUTHOR, hostVersionsFrom } from "../src/versions.ts";

interface Printed {
	message: string;
	type: string | undefined;
}

function harness(client: Partial<CloudThinkerClient>, enabled: boolean, canEdit: boolean) {
	const printed: Printed[] = [];
	const runtime = new CloudThinkerRuntime(
		{ appendEntry: () => {} } as unknown as ExtensionAPI,
		client as CloudThinkerClient,
		hostVersionsFrom({ version: "0.4.0", piVersion: "0.85.1" }, "0.4.0"),
	);
	runtime.session = {
		conversation_id: "c-1",
		workspace_id: "w-1",
		web_url: "https://web/c-1",
		auto_mode: { enabled, can_edit: canEdit },
	};
	runtime.setAutoMode({ enabled, canEdit });
	const ui = { notify: (message: string, type?: string) => printed.push({ message, type }) };
	return { runtime, ui, printed };
}

test("auto with no argument prints the mode and who can change it", async () => {
	const editor = harness({}, false, true);
	await autoCommand(editor.runtime, editor.ui, "");
	assert.deepEqual(editor.printed, [
		{
			message: [
				"Approval mode: Manual — every cloud write waits for a human",
				"change it with /cloudthinker auto on|off",
			].join("\n"),
			type: "info",
		},
	]);
	assert.deepEqual(autoModeLines({ enabled: true, canEdit: false }), [
		"Approval mode: Auto — a cloud write runs when the workspace rule allows it",
		AUTO_MODE_EDITOR_HINT,
	]);
});

test("auto on and off patch the workspace and update the session's mode", async () => {
	const patched: { workspace: string; enabled: boolean }[] = [];
	const client = {
		setWorkspaceAutoMode: async (workspace: string, enabled: boolean) => {
			patched.push({ workspace, enabled });
			return { enabled };
		},
	};
	const editor = harness(client, false, true);
	await autoCommand(editor.runtime, editor.ui, "on");
	assert.equal(editor.runtime.autoMode?.enabled, true);
	assert.equal(editor.runtime.autoMode?.canEdit, true);
	await autoCommand(editor.runtime, editor.ui, "off");
	assert.equal(editor.runtime.autoMode?.enabled, false);
	assert.deepEqual(patched, [
		{ workspace: "w-1", enabled: true },
		{ workspace: "w-1", enabled: false },
	]);
	assert.deepEqual(
		editor.printed.map((entry) => entry.message),
		[
			"Approval mode: Auto — a cloud write runs when the workspace rule allows it",
			"Approval mode: Manual — every cloud write waits for a human",
		],
	);
});

test("a 403 on the switch names the settings editor and leaves the mode alone", async () => {
	const client = {
		setWorkspaceAutoMode: async () => {
			throw new CloudThinkerApiError(403, "forbidden");
		},
	};
	const viewer = harness(client, false, false);
	await autoCommand(viewer.runtime, viewer.ui, "on");
	assert.equal(viewer.runtime.autoMode?.enabled, false);
	assert.deepEqual(viewer.printed, [{ message: AUTO_MODE_EDITOR_HINT, type: "warning" }]);
});

test("a write verdict keeps the session's mode current", () => {
	const session = harness({}, true, false);
	session.runtime.noteWriteVerdict("auto_mode_disabled");
	assert.equal(session.runtime.autoMode?.enabled, false);
	session.runtime.noteWriteVerdict("workspace_trusted_command");
	assert.equal(session.runtime.autoMode?.enabled, true);
});

test("notify resends the approval for the ask thread, and says so when nothing waits", async () => {
	const resent: string[] = [];
	const client = {
		resendInterruptNotification: async (conversationId: string) => {
			resent.push(conversationId);
		},
	};
	const linked = harness(client, false, false);
	await notifyApprovers(linked.runtime, linked.ui);
	assert.deepEqual(linked.printed, [{ message: NOTHING_WAITING, type: "info" }]);

	linked.runtime.askThread = { conversation_id: "h-1", web_url: "https://web/h-1" };
	await notifyApprovers(linked.runtime, linked.ui);
	assert.deepEqual(resent, ["h-1"]);
	assert.deepEqual(linked.printed.at(-1), { message: APPROVERS_NOTIFIED, type: "info" });

	const parkedNothing = harness(
		{
			resendInterruptNotification: async () => {
				throw new CloudThinkerApiError(404, "No pending interrupt found for this conversation.");
			},
		},
		false,
		false,
	);
	parkedNothing.runtime.askThread = { conversation_id: "h-1", web_url: "https://web/h-1" };
	await notifyApprovers(parkedNothing.runtime, parkedNothing.ui);
	assert.deepEqual(parkedNothing.printed, [{ message: NOTHING_WAITING, type: "info" }]);

	const failing = harness(
		{
			resendInterruptNotification: async () => {
				throw new CloudThinkerApiError(500, "boom");
			},
		},
		false,
		false,
	);
	failing.runtime.askThread = { conversation_id: "h-1", web_url: "https://web/h-1" };
	await notifyApprovers(failing.runtime, failing.ui);
	assert.deepEqual(failing.printed, [{ message: "boom", type: "error" }]);
});

test("about names both versions, the config directory, and the pi attribution", () => {
	const versions = hostVersionsFrom(
		{ version: "0.4.0", piVersion: "0.85.1", piRepository: "https://github.com/earendil-works/pi" },
		"0.4.0",
	);
	const printed = aboutLines(versions, "/home/dev/.cloudthinker/agent", "https://web/c-1").join(
		"\n",
	);
	assert.ok(printed.includes("cloudthinker v0.4.0"));
	assert.ok(printed.includes("pi v0.85.1"));
	assert.ok(printed.includes(`built on pi by ${PI_AUTHOR}, MIT`));
	assert.ok(printed.includes("https://github.com/earendil-works/pi"));
	assert.ok(printed.includes("/home/dev/.cloudthinker/agent"));
	assert.ok(printed.includes("https://web/c-1"));
});

test("about says so when the session never linked", () => {
	const versions = hostVersionsFrom({ version: "0.4.0", piVersion: "0.85.1" }, "0.4.0");
	assert.ok(aboutLines(versions, "/agent", undefined).includes("Session: not linked"));
});

test("the terminal title carries the directory, and the workspace once identity arrives", () => {
	assert.equal(sessionTitle("/home/dev/infra", undefined), "cloudthinker · infra");
	assert.equal(sessionTitle("/home/dev/infra", "acme-prod"), "cloudthinker · infra · acme-prod");
});
