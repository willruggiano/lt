.DEFAULT_GOAL := help

.PHONY: help build check fix

help: ## List available targets
	@grep -hE '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | awk 'BEGIN{FS=":.*?## "}{printf "  %-8s %s\n", $$1, $$2}'

build: ## Build the binary
	cargo build

check: ## Run formatter, linter, supply-chain, dedup, and test gates
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings
	GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null cargo deny check
	cargo machete
	cpd .
	cargo dupes check --exclude-tests --min-nodes 25 --max-exact 0 --max-near 0
	cargo test

fix: ## Apply the fixable variants of the check gates
	cargo fmt
	cargo clippy --all-targets --fix --allow-dirty --allow-staged
	cargo machete --fix
