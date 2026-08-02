.PHONY: test test-unit test-integration build run

test:
	cargo test -p fleet-orchestrator

test-unit:
	cargo test -p fleet-orchestrator --lib

test-integration:
	@test -n "$$DATABASE_URL" || (echo "Set DATABASE_URL"; exit 1)
	cargo test -p fleet-orchestrator --test store_integration --test tick_integration

build:
	cargo build -p fleet-orchestrator --release

run:
	@echo "WARNING: single-process leadership — do not run multiple replicas against the same DB"
	cargo run -p fleet-orchestrator
