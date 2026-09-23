# CloudThinker CLI — codegen + verification.
#
# `make gen`   regenerates the committed OpenAPI snapshot and the generated
#              `cloudthinker-api` crate from the live backend app. Idempotent:
#              a second run produces no diff.
# `make check` runs the same gates CI does: fmt --check, clippy -D warnings
#              (workspace lints active), and the full test suite.

CLI_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST))))
BACKEND_DIR := $(abspath $(CLI_DIR)/../backend)
API_CRATE := $(CLI_DIR)/crates/cloudthinker-api
SNAPSHOT := $(CLI_DIR)/openapi/cloudthinker-cli.json
BUILD := $(CLI_DIR)/.gen

# Nightly rustfmt: progenitor emits unstable fmt options that stable rustfmt panics on.
NIGHTLY_RUSTFMT := $(shell rustup which rustfmt --toolchain nightly)

.PHONY: gen check fmt clippy test clean-gen release-sync check-release changelog

gen:
	@mkdir -p $(BUILD) $(CLI_DIR)/openapi
	@echo ">> dump live OpenAPI spec (in-process, no server)"
	cd $(BACKEND_DIR) && POSTGRES_SERVER=localhost REDIS_SERVER=localhost \
		uv run python -c "import json; from app.main import app; json.dump(app.openapi(), open('$(BUILD)/full.json','w'))"
	@echo ">> prune to CLI allowlist + schema closure"
	python3 $(CLI_DIR)/scripts/prune_spec.py $(BUILD)/full.json $(BUILD)/pruned-3.1.json
	@echo ">> down-convert 3.1 -> 3.0"
	cd $(CLI_DIR)/../frontend && npx --yes @apiture/openapi-down-convert@0.14.1 \
		--input $(BUILD)/pruned-3.1.json --output $(BUILD)/pruned-3.0.json
	@echo ">> fixup 3.0 gaps -> committed snapshot"
	python3 $(CLI_DIR)/scripts/fixup_spec.py $(BUILD)/pruned-3.0.json $(SNAPSHOT)
	@echo ">> progenitor -> cloudthinker-api crate"
	rm -rf $(API_CRATE)
	RUSTFMT="$(NIGHTLY_RUSTFMT)" cargo progenitor \
		--input $(SNAPSHOT) --output $(API_CRATE) \
		--name cloudthinker-api --version 0.1.0 --license-name Apache-2.0
	@echo ">> inject relaxed-lint header into generated lib.rs"
	python3 $(CLI_DIR)/scripts/inject_lint_header.py $(API_CRATE)/src/lib.rs
	@echo ">> gen complete: $(SNAPSHOT) + $(API_CRATE)"

check: fmt clippy test

# The generated `cloudthinker-api` crate is formatted by progenitor's nightly
# rustfmt (which stable `cargo fmt` disagrees with), so the fmt gate covers only
# the hand-written crates.
fmt:
	cd $(CLI_DIR) && cargo fmt --check -p cloudthinker-client -p cloudthinker-cli

clippy:
	cd $(CLI_DIR) && cargo clippy --all-targets -- -D warnings

test:
	cd $(CLI_DIR) && cargo test
	python3 $(CLI_DIR)/scripts/test_check_release.py
	python3 $(CLI_DIR)/scripts/test_changelog.py

changelog:
	python3 $(CLI_DIR)/scripts/changelog.py fold --version $(VERSION)

clean-gen:
	rm -rf $(BUILD)

# Publish this cli/ tree to the private GitHub source repo
# (cloudthinker-ai/cloudthinker-cli-src), whose workflow releases to the public
# cloudthinker-ai/cloudthinker-cli. One-directional mirror; the monorepo is the
# source of truth. Needs a gh account with push access. See scripts/release-sync.sh.
release-sync:
	bash $(CLI_DIR)/scripts/release-sync.sh

# Prove the latest public release is complete and downloadable without a token.
# TAG=vX.Y.Z checks that tag instead of latest. See scripts/check-release.sh.
check-release:
	bash $(CLI_DIR)/scripts/check-release.sh
