default: help

.PHONY: help
help: # Show help for each of the Makefile recipes.
	@grep -E '^[a-zA-Z0-9 -]+:.*#'  Makefile | sort | while read -r l; do printf "\033[1;32m$$(echo $$l | cut -f 1 -d':')\033[00m:$$(echo $$l | cut -f 2- -d'#')\n"; done

.PHONY: prepare
prepare: # Prepare the app
	./download-ggml-model.sh base
	cargo build --release

.PHONY: run
run: # Run the app
	ALLOW_PROCESS=1 RUST_BACKTRACE=full ./target/release/app

.PHONY: format
format: # Format the app
	cargo clippy --fix --allow-dirty --allow-no-vcs
	cargo fmt

.PHONY: check
check: # Check the app
	cargo fmt -- --check
	cargo clippy -- -D warnings
