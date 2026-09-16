import assert from "node:assert/strict";
import test from "node:test";
import { stripVTControlCharacters } from "node:util";
import { initTheme } from "@earendil-works/pi-coding-agent";
import type { Theme } from "@earendil-works/pi-coding-agent";
import type { TUI } from "@earendil-works/pi-tui";
import { visibleWidth } from "@earendil-works/pi-tui";

import {
	LINKING_LINE,
	UNLINKED_LINE,
	type HeaderState,
	type HeaderStyler,
	formatHeaderText,
	SessionHeader,
} from "../src/header.ts";
import { LOGO_WIDTH, renderLogo } from "../src/logo.ts";
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
	return formatHeaderText(state, versions, plain, { compact: "", expanded: "", more: "" }, false, 60).split("\n");
}

test("the product line names the host version and credits pi and its author", () => {
	const header = lines({ link: "linking" }).join("\n");
	assert.match(header, /CloudThinker v0\.4\.0/);
	assert.match(header, /built on pi v0\.85\.1 by Mario Zechner \(MIT\)/);
});

test("before the session exists the second line reads as linking", () => {
	assert.equal(lines({ link: "linking" })[1], LINKING_LINE);
});

test("a linked session names the workspace, the user, and the mirror", () => {
	const header = lines({
		link: "linked",
		workspaceName: "acme-prod",
		userEmail: "dev@acme.io",
		webUrl: "https://app.cloudthinker.io/chat/c-1",
	});
	assert.equal(header[1], "acme-prod");
	assert.equal(header[2], "dev@acme.io");
	assert.match(header[3]!, /\/open/);
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
		"acme-prod · Auto",
	);
	assert.equal(
		lines({ ...base, autoMode: false })[1],
		"acme-prod · Manual",
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

test("the live header responds to resizing without losing identity or failure state", () => {
	initTheme("dark");
	const header = new SessionHeader(versions);
	const theme = plain as Theme;
	const component = header.factory({ requestRender() {} } as unknown as TUI, theme);
	header.set({ link: "linked", workspaceName: "Workspace", userEmail: "person@example.com", webUrl: "https://example.com/session", autoMode: false });
	for (const width of [120, 80, 60, 32, 80]) {
		const rows = component.render(width);
		assert.ok(rows.every((row) => visibleWidth(row) <= width));
		const text = stripVTControlCharacters(rows.join("\n"));
		assert.match(text, /CloudThinker/);
		assert.match(text, /person@example.com/);
		assert.equal(/[▀▄█]/.test(text), width >= 78);
	}
	component.setExpanded(true);
	assert.match(stripVTControlCharacters(component.render(80).join("\n")), /https:\/\/example.com\/session/);
	header.set({ link: "unavailable" });
	assert.match(stripVTControlCharacters(component.render(80).join("\n")), /cloud tools unavailable/);
});

test("monochrome logo preserves its silhouette without escape sequences", () => {
	const mono = renderLogo(plain, true);
	assert.deepEqual(mono, mono.map(stripVTControlCharacters));
	assert.ok(mono.some((row) => /[█]/.test(row)));
	assert.ok(mono.every((row) => visibleWidth(row) <= LOGO_WIDTH));
	assert.ok(mono.some((row) => visibleWidth(row) === LOGO_WIDTH));
});

test("the logo colors its rows with the brand gradient in truecolor", (t) => {
	const previous = process.env.NO_COLOR;
	t.after(() => { if (previous === undefined) delete process.env.NO_COLOR; else process.env.NO_COLOR = previous; });
	delete process.env.NO_COLOR;
	const rows = renderLogo(plain);
	assert.equal(rows.length, 12);
	assert.ok(rows[0]!.startsWith("\u001b[38;2;142;197;235m"));
	assert.ok(rows[rows.length - 1]!.startsWith("\u001b[38;2;43;181;168m"));
	for (const row of rows) assert.ok(row.endsWith("\u001b[39m"));
	assert.deepEqual(rows.map(stripVTControlCharacters), renderLogo(plain, true));
	const header = formatHeaderText({ link: "linking" }, versions, plain,
		{ compact: "", expanded: "", more: "" }, false, 80);
	assert.ok(header.includes("\u001b[38;2;142;197;235m"));
});

test("a light theme darkens the gradient so every row stays readable", async () => {
	const { getThemeByName } = await import(new URL("./modes/interactive/theme/theme.js", import.meta.resolve("@earendil-works/pi-coding-agent")).href);
	initTheme("dark");
	const light = getThemeByName("light")!;
	const rows = renderLogo(light);
	assert.ok(rows[0]!.startsWith("\u001b[38;2;26;111;158m"));
	assert.ok(rows[rows.length - 1]!.startsWith("\u001b[38;2;0;120;111m"));
	const luminance = (color: readonly [number, number, number]) => {
		const channel = (value: number) => {
			const c = value / 255;
			return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
		};
		return 0.2126 * channel(color[0]) + 0.7152 * channel(color[1]) + 0.0722 * channel(color[2]);
	};
	const ratio = (a: readonly [number, number, number], b: readonly [number, number, number]) => {
		const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
		return (hi! + 0.05) / (lo! + 0.05);
	};
	for (const stop of [[26, 111, 158], [0, 120, 111]] as const) {
		assert.ok(ratio(stop, [245, 245, 245]) >= 3, `${stop} below 3:1 on #f5f5f5`);
		assert.ok(ratio(stop, [255, 255, 255]) >= 3, `${stop} below 3:1 on #ffffff`);
	}
});

test("NO_COLOR bypasses logo styling at the header boundary", (t) => {
	const previous = process.env.NO_COLOR;
	t.after(() => { if (previous === undefined) delete process.env.NO_COLOR; else process.env.NO_COLOR = previous; });
	process.env.NO_COLOR = "1";
	const styler: HeaderStyler = { ...plain, fg: (_color, text) => `\u001b[31m${text}\u001b[39m` };
	const header = formatHeaderText({ link: "linking" }, versions, styler,
		{ compact: "", expanded: "", more: "" }, false, 80);
	const rows = header.split("\n");
	assert.equal(rows[0]!.trimEnd(), renderLogo(plain, true)[0]!.trimEnd());
	assert.ok(rows.slice(0, renderLogo(plain, true).length).every((row) => !row.includes("\u001b")));
});

test("an unknown palette color falls back to the dark stops instead of crashing", () => {
	const ansiTheme: HeaderStyler = {
		...plain,
		getFgAnsi: (color) => color === "text" ? "\u001b[38;5;5m" : "",
	};
	const rows = renderLogo(ansiTheme);
	assert.ok(rows[0]!.startsWith("\u001b[38;2;142;197;235m"));
	assert.deepEqual(rows.map(stripVTControlCharacters), renderLogo(plain, true));
});

test("the wide header stacks the gradient wordmark above the identity lines", () => {
	const header = formatHeaderText(
		{ link: "linked", workspaceName: "acme-prod", userEmail: "dev@acme.io", webUrl: "https://app.cloudthinker.io/chat/c-1" },
		versions, plain, { compact: "", expanded: "", more: "" }, false, 80);
	const rows = header.split("\n");
	const colored = renderLogo(plain);
	assert.deepEqual(rows.slice(0, colored.length), colored);
	assert.equal(rows[colored.length], "");
	assert.match(stripVTControlCharacters(rows[colored.length + 1]!), /^CloudThinker v/);
	assert.equal(stripVTControlCharacters(rows[colored.length + 2]!).trimEnd(), "acme-prod");
});
