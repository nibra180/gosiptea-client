.PHONY: build test check smoke install update

build:
	cargo build --locked --release

test:
	cargo test --locked --all-targets

check:
	cargo fmt --all -- --check
	cargo test --locked --all-targets
	cargo clippy --locked --all-targets -- -D warnings

smoke:
	cargo test --locked --release --test live_start -- --ignored

install update:
	bash scripts/install-local.sh
