.PHONY: test test-unit test-integration build run tidy fmt

test:
	cargo test

test-unit:
	cargo test --lib

test-integration:
	@test -n "$$DATABASE_URL" || (echo "Set DATABASE_URL (see .env.example)"; exit 1)
	cargo test --test store_integration

build:
	cargo build --release

run:
	@echo "WARNING: single-process leadership — do not run multiple replicas against the same DB"
	cargo run

fmt:
	cargo fmt

tidy:
	cargo fetch
