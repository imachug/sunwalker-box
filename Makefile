.PHONY: sunwalker_box

all: sunwalker_box

sunwalker_box:
	cargo +nightly build --target=x86_64-unknown-linux-musl -Z build-std=std,panic_abort --release
	cp target/x86_64-unknown-linux-musl/release/sunwalker_box sunwalker_box