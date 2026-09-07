import assert from "node:assert/strict";
import test from "node:test";

import { sanitizeRemote } from "../src/location.ts";

test("an https remote loses its userinfo before it reaches the mirror", () => {
	assert.equal(
		sanitizeRemote("https://oauth2:glpat-EXAMPLE-NOT-REAL@gitlab.com/group/repo.git"),
		"https://gitlab.com/group/repo.git",
	);
	assert.equal(
		sanitizeRemote("http://someone:secret-placeholder@example.com/group/repo.git"),
		"http://example.com/group/repo.git",
	);
	assert.equal(
		sanitizeRemote("https://x-access-token-placeholder@github.com/group/repo.git"),
		"https://github.com/group/repo.git",
	);
});

test("a login name that is not a secret survives", () => {
	assert.equal(
		sanitizeRemote("ssh://git@gitlab.com/group/repo.git"),
		"ssh://git@gitlab.com/group/repo.git",
	);
	assert.equal(sanitizeRemote("git@github.com:group/repo.git"), "git@github.com:group/repo.git");
	assert.equal(
		sanitizeRemote("https://gitlab.com/group/repo.git"),
		"https://gitlab.com/group/repo.git",
	);
	assert.equal(sanitizeRemote("/srv/git/repo.git"), "/srv/git/repo.git");
	assert.equal(sanitizeRemote(null), null);
});

test("an at sign in the path is not mistaken for userinfo", () => {
	assert.equal(
		sanitizeRemote("https://example.com/group/repo@v2.git"),
		"https://example.com/group/repo@v2.git",
	);
});
