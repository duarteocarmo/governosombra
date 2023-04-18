.PHONY: prepare run format

prepare:
	./download-ggml-model.sh base
	cargo build --release
run:
	./target/release/app

format: 
	rustfmt src/*

