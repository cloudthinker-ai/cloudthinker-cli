import assert from "node:assert/strict";
import test from "node:test";

import {
	LINKING_LINE,
	UNLINKED_LINE,
	type HeaderState,
	type HeaderStyler,
	formatHeaderText,
	formatIdentityLines,
} from "../src/header.ts";
import { hostVersionsFrom, normalizeRepositoryUrl } from "../src/versions.ts";

const plain: HeaderStyler = {
	fg: (_color, text) => text,
	bold: (text) => text,
};

const versions = hostVersionsFrom(
	{ version: "0.4.0", piVersion: "0.85.1", piRepository: "https://github.com/earendil-works/pi" },
	"0.4.0",
);

function lines(state: HeaderState): string[] {
	return formatIdentityLines(state, versions, plain);
}

test("the product line names the host version and credits pi and its author", () => {
	const [product] = lines({ link: "linking" });
	assert.equal(product, "cloudthinker v0.4.0  built on pi v0.85.1 by Mario Zechner (MIT)");
});

test("before the session exists the second line reads as linking", () => {
	assert.equal(lines({ link: "linking" })[1], LINKING_LINE);
});

test("a linked session names the workspace, the user, and the mirror", () => {
	const [, session] = lines({
		link: "linked",
		workspaceName: "acme-prod",
		userEmail: "dev@acme.io",
		webUrl: "https://app.cloudthinker.io/chat/c-1",
	});
	assert.equal(
		session,
		"acme-prod · dev@acme.io · mirrored → https://app.cloudthinker.io/chat/c-1",
	);
});

test("the session line ends with the workspace approval mode once it is known", () => {
	const base = {
		link: "linked" as const,
		workspaceName: "acme-prod",
		userEmail: "dev@acme.io",
		webUrl: "https://app.cloudthinker.io/chat/c-1",
	};
	assert.equal(
		lines({ ...base, autoMode: true })[1],
		"acme-prod · dev@acme.io · mirrored → https://app.cloudthinker.io/chat/c-1 · Auto",
	);
	assert.equal(
		lines({ ...base, autoMode: false })[1],
		"acme-prod · dev@acme.io · mirrored → https://app.cloudthinker.io/chat/c-1 · Manual",
	);
});

test("a session that could not be created says the cloud tools are gone", () => {
	assert.equal(lines({ link: "unavailable" })[1], UNLINKED_LINE);
});

test("the compact form carries the hints and the expand prompt, the expanded form does not", () => {
	const hints = { compact: "compact hints", expanded: "expanded hints", more: "Press ctrl+t" };
	const collapsed = formatHeaderText({ link: "linking" }, versions, plain, hints, false);
	assert.ok(collapsed.endsWith("compact hints\nPress ctrl+t"));
	const expanded = formatHeaderText({ link: "linking" }, versions, plain, hints, true);
	assert.ok(expanded.endsWith("expanded hints"));
	assert.ok(!expanded.includes("Press ctrl+t"));
});

test("the header never repeats pi's own onboarding sentence", () => {
	const hints = { compact: "c", expanded: "e", more: "m" };
	const text = formatHeaderText({ link: "linking" }, versions, plain, hints, false);
	assert.ok(!text.includes("Pi can explain its own features"));
});

test("the pi version and repository come from the sidecar, or from pi's own manifest", () => {
	const sidecar = hostVersionsFrom(
		{ version: "0.4.0", piVersion: "0.85.1", piRepository: "https://github.com/earendil-works/pi" },
		"0.4.0",
	);
	assert.deepEqual(sidecar, {
		host: "0.4.0",
		pi: "0.85.1",
		piRepositoryUrl: "https://github.com/earendil-works/pi",
	});

	const own = hostVersionsFrom(
		{
			version: "0.85.1",
			repository: { url: "git+https://github.com/earendil-works/pi.git" },
		},
		"0.85.1",
	);
	assert.equal(own.pi, "0.85.1");
	assert.equal(own.piRepositoryUrl, "https://github.com/earendil-works/pi");
});

test("a git url loses its scheme prefix and its .git suffix", () => {
	assert.equal(
		normalizeRepositoryUrl("git+https://github.com/earendil-works/pi.git"),
		"https://github.com/earendil-works/pi",
	);
});
