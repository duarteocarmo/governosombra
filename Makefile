.PHONY: prepare run

prepare:
	./download-ggml-model.sh base
	cargo build --release
run:
	./target/release/app

format: 
	rustfmt src/

