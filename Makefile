.PHONY: prepare run format

prepare:
	./download-ggml-model.sh base
	cargo build --release

run:
	ALLOW_PROCESS=1 RUST_BACKTRACE=full ./target/release/app

format: 
	rustfmt src/*

