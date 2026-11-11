.DEFAULT_GOAL := build

build:
	cargo build --release --target wasm32-unknown-unknown
	mkdir -p out
	cp target/wasm32-unknown-unknown/release/hr_enrollment.wasm out/plugin.wasm

clean:
	cargo clean
	rm -rf out

.PHONY: build clean
