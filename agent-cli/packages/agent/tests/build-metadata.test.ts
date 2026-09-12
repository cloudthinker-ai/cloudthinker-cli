import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { cpSync, mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

test("CA-AD-5 release packaging identifies monorepo source and rejects malformed metadata", () => {
	const root = mkdtempSync(join(tmpdir(), "ct-build-metadata-"));
	try {
		const pkg = join(root, "agent-cli/packages/agent");
		mkdirSync(pkg, { recursive: true });
		for (const path of ["scripts", "themes", "package.json"]) {
			cpSync(new URL(`../${path}`, import.meta.url), join(pkg, path), { recursive: true });
		}
		symlinkSync(new URL("../node_modules", import.meta.url), join(pkg, "node_modules"), "dir");
		const compiler = join(root, "fixture-compiler");
		writeFileSync(compiler, '#!/bin/sh\nwhile [ "$1" != "--outfile" ]; do shift; done\nprintf fixture > "$2"\n', { mode: 0o755 });
		const git = (...args: string[]) => execFileSync("git", ["-C", root, ...args], { encoding: "utf8" }).trim();
		git("init", "-q");
		git("add", ".");
		git("-c", "user.name=fixture", "-c", "user.email=fixture@example.invalid", "commit", "-qm", "fixture");
		const out = join(root, "output");
		const build = () => spawnSync("bash", [join(pkg, "scripts/build.sh"), "0.5.5", "--host", "--out", out], {
			env: { ...process.env, BUN: compiler }, encoding: "utf8",
		});
		const revision = "a".repeat(40);
		writeFileSync(join(root, "agent-cli/.source-revision"), `${revision}\n`);
		assert.notEqual(git("rev-parse", "HEAD"), revision);
		const result = build();
		assert.equal(result.status, 0, result.stderr);
		const triple = process.platform === "darwin"
			? `${process.arch === "arm64" ? "aarch64" : "x86_64"}-apple-darwin`
			: `${process.arch === "arm64" ? "aarch64" : "x86_64"}-unknown-linux-gnu`;
		const manifest = JSON.parse(execFileSync("tar", ["-xOf", join(out, `cloudthinker-agent-${triple}.tar.gz`), "cloudthinker-agent/package.json"], { encoding: "utf8" }));
		assert.equal(manifest.buildId, revision);
		execFileSync("sha256sum", ["-c", "cloudthinker-agent-sha256.sum"], { cwd: out });
		writeFileSync(join(root, "agent-cli/.source-revision"), "invalid\n");
		const invalid = build();
		assert.notEqual(invalid.status, 0);
		assert.match(invalid.stderr, /invalid monorepo source revision/);
	} finally {
		rmSync(root, { recursive: true, force: true });
	}
});
