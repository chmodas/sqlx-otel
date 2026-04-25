.PHONY: fmt lint test test-sqlite test-postgres test-mysql test-pool-metrics test-pool-metrics-async-std coverage clean

fmt:
	cargo +nightly fmt

lint:
	cargo clippy --all-targets --all-features -- -D warnings

test:
	cargo test --all-features
	cargo test --features "sqlite,runtime-async-std" --test pool_metrics

test-sqlite:
	cargo test --all-features --test sqlite

test-postgres:
	cargo test --all-features --test postgres

test-mysql:
	cargo test --all-features --test mysql

test-pool-metrics:
	cargo test --all-features --test pool_metrics

test-pool-metrics-async-std:
	cargo test --features "sqlite,runtime-async-std" --test pool_metrics

COV_REPORT ?= --summary-only

coverage:
	cargo llvm-cov clean
	cargo llvm-cov --all-features --no-report
	cargo llvm-cov --features "sqlite,runtime-async-std" --test pool_metrics --no-report
	cargo llvm-cov report $(COV_REPORT)
