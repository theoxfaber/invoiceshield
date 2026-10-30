# InvoiceShield developer entry points. Run from the repo root.
#   make test         Rust (fmt on ops files, clippy -D warnings, cargo test) + ruff + pytest
#   make demo         offline replay of the recorded dev-set runs: no API key, no network
#   make eval         re-run the no-model evals (hijacked-agent gate, sequences, benign, clean set)
#   make load         load test of POST /v1/actions -> eval/results/load-*.json
#   make demo-pages   regenerate ui/demo-v3.html from ui/build/ (must be byte-identical)
#   make audit        cargo audit + pip-audit
#   make docker       build the container image
PY      ?= python3
CARGO   ?= cargo
GOV     := governor
# Honour CARGO_TARGET_DIR (absolute path) when set; the Python side finds the binary via GOVERNOR_BIN.
TARGET  := $(or $(CARGO_TARGET_DIR),$(GOV)/target)
BIN     := $(TARGET)/release/governor-server
export GOVERNOR_BIN := $(abspath $(BIN))
OPS_RS  := $(GOV)/src/config.rs $(GOV)/src/observability.rs

.PHONY: all build test test-rust test-py lint fmt-check demo demo-check eval load demo-pages audit docker clean

all: test

build: $(BIN)

$(BIN): $(wildcard $(GOV)/src/*.rs $(GOV)/src/bin/*.rs) $(GOV)/Cargo.toml $(GOV)/Cargo.lock
	cd $(GOV) && $(CARGO) build --release --locked

fmt-check:
	rustfmt --edition 2021 --check $(OPS_RS)

test-rust: fmt-check
	cd $(GOV) && $(CARGO) clippy --all-targets --locked -- -D warnings
	cd $(GOV) && $(CARGO) test --locked

lint:
	ruff check .

test-py: build lint
	$(PY) -m pytest

test: test-rust test-py

demo: build
	$(PY) -m eval.demo

demo-check: build
	$(PY) -m eval.demo --strict

eval: build
	$(PY) -m eval.run_gate_only
	$(PY) -m eval.run_sequences
	$(PY) -m eval.run_benign
	$(PY) -m eval.run_clean

load: build
	$(PY) -m eval.load_test

demo-pages:
	$(PY) -m ui.build_demo
	$(PY) -m ui.build.build_demo_v2
	$(PY) -m ui.build.build_v3
	git diff --stat --exit-code ui/demo-v3.html

audit:
	cd $(GOV) && cargo audit
	pip-audit -r requirements.txt

docker:
	docker build -t invoiceshield:local .

clean:
	rm -rf ui/build/out .pytest_cache
