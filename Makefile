.PHONY: build test fmt lint run docker chart-package

build:
	cargo build --release

test:
	cargo test --workspace

fmt:
	cargo fmt --all

lint:
	cargo clippy --workspace --all-targets -- -D warnings

run:
	cargo run --package gh-token-server

docker:
	docker build -t ghcr.io/runningman84/gh-token:dev .

chart-package:
	helm package chart
