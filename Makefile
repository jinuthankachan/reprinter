# reprinter developer commands. Run `make` to list them.
#
# Building needs Rust (https://rustup.rs). `release` and `test-web` build
# static binaries and need rustup, which adds the musl targets by itself.
# The test-install and test-web targets need Docker.

SHELL := bash
.SHELLFLAGS := -eu -o pipefail -c
.DEFAULT_GOAL := help

CARGO        ?= cargo
ARCHS        ?= amd64 arm64 armhf
IMAGES       ?=
PORT         ?= 8631
INSTALL_ARGS ?=
PURGE        ?=

BIN     := web/target/release/reprinter-web
# uninstall runs the installed binary, or this checkout's build once that is
# gone (e.g. to purge after a plain uninstall).
INSTALLED := $(firstword $(wildcard /usr/local/bin/reprinter-web $(BIN)))
VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' web/Cargo.toml | head -n 1)
SCRIPTS := install.sh pi-setup.sh lib/*.sh drivers/*.sh tests/*.sh web/build-release.sh

.PHONY: help build release run test lint fmt check test-install test-install-source \
        test-web test-all install uninstall tag clean

help: ## Show this list
	@printf 'reprinter %s\n\nUsage: make <target> [VARIABLE=value ...]\n\nTargets:\n' '$(VERSION)'
	@awk 'BEGIN { FS = ":.*## " } /^[a-z][a-z-]*:.*## / { printf "  %-20s %s\n", $$1, $$2 }' $(MAKEFILE_LIST)
	@printf '\nVariables:\n'
	@printf '  %-20s %s\n' \
	  'ARCHS=...' 'architectures for release (default: amd64 arm64 armhf)' \
	  'IMAGES=...' 'Docker images for the test-* targets (default: each test script'"'"'s set)' \
	  'PORT=...' 'port for run (default: 8631)' \
	  'INSTALL_ARGS=...' 'options for install, e.g. "--printer canon-lbp1210 --paper letter"' \
	  'PURGE=1' 'uninstall also removes the settings and the reprinter user'

build: ## Build reprinter-web for this machine (web/target/release/reprinter-web)
	cd web && $(CARGO) build --release --locked

release: ## Build the static release binaries and SHA256SUMS into web/dist/
	web/build-release.sh $(ARCHS)

run: ## Run the web interface on 127.0.0.1:PORT against this machine's CUPS
	cd web && $(CARGO) run -- --share-dir .. --config dev/config.toml --listen 127.0.0.1:$(PORT)

test: ## Unit tests
	cd web && $(CARGO) test --locked

lint: ## Formatting, clippy and shell syntax (plus shellcheck when installed)
	cd web && $(CARGO) fmt --check
	cd web && $(CARGO) clippy --locked --all-targets -- -D warnings
	for f in $(SCRIPTS); do bash -n "$$f"; done
	for d in $$(sed -n 's/^ *DEB_SHA256=//p' pi-setup.sh); do \
	  grep -q "$$d" drivers/captppd.sh || { echo "pi-setup.sh: digest $$d is not in drivers/captppd.sh" >&2; exit 1; }; done
	if command -v shellcheck >/dev/null; then shellcheck -x $(SCRIPTS); \
	else echo "shellcheck not installed: skipped"; fi
	if command -v node >/dev/null; then node --check web/static/app.js; fi

fmt: ## Format the Rust code
	cd web && $(CARGO) fmt

check: lint test ## lint, then test

test-install: ## install.sh in Docker containers (no printer needed)
	tests/container-test.sh $(IMAGES)

test-install-source: ## install.sh with the driver built from source, in Docker
	FROM_SOURCE=1 tests/container-test.sh $(or $(IMAGES),debian:13)

test-web: ## reprinter-web end to end in Docker containers (no printer needed)
	tests/web-test.sh $(IMAGES)

test-all: check test-install test-web ## All of the above

install: build ## Set up this host: print server and web interface (sudo)
	sudo $(BIN) install $(INSTALL_ARGS)

uninstall: ## Remove the web interface service from this host (sudo); PURGE=1 also its settings and user
	@test -n "$(INSTALLED)" || { echo "no reprinter-web binary: run make build first" >&2; exit 1; }
	sudo $(INSTALLED) uninstall $(if $(PURGE),--purge)

tag: ## Tag the version in web/Cargo.toml for a release (does not push)
	@test -z "$$(git status --porcelain)" || { echo "commit your changes first" >&2; exit 1; }
	@if git rev-parse -q --verify "refs/tags/v$(VERSION)" >/dev/null; then \
	  echo "v$(VERSION) already exists: bump version in web/Cargo.toml" >&2; exit 1; fi
	git tag -a "v$(VERSION)" -m "reprinter-web $(VERSION)"
	@echo "Publish it (CI builds and attaches the binaries): git push origin v$(VERSION)"

clean: ## Remove build output, release binaries and the local run's data
	cd web && $(CARGO) clean
	rm -rf web/dist web/dev
