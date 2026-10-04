# All paths are relative: the repository may live in a directory whose name
# contains spaces or non-ASCII characters.
CARGO ?= cargo
PY ?= python3
JOBS ?= 4
VENV := .venv
VPY := $(VENV)/bin/python

.PHONY: build test lint bench quick repro figures mutants clean

build:
	$(CARGO) build --release -j $(JOBS)

test:
	$(CARGO) test --release -j $(JOBS)

lint:
	$(CARGO) fmt --check
	$(CARGO) clippy --release -j $(JOBS) --all-targets -- -D warnings
	$(PY) -m py_compile analysis/run_experiments.py analysis/figures.py analysis/page.py tools/mutants.py

$(VENV)/.ok: requirements.txt
	$(PY) -m venv $(VENV)
	$(VPY) -m pip install -q -r requirements.txt
	touch $(VENV)/.ok

# Scaled-down experiment set (32-host FatTree, shorter windows): about 15 s.
quick: build $(VENV)/.ok
	$(PY) analysis/run_experiments.py --set quick --jobs $(JOBS)
	$(VPY) analysis/figures.py --results results/quick --figures results/quick/figures \
		--html results/quick/index.html --numbers results/quick/numbers.json --label quick

# Full experiment set on the paper's 320-host FatTree; see README for runtime.
repro: build $(VENV)/.ok
	$(PY) analysis/run_experiments.py --set full --jobs $(JOBS)
	$(MAKE) figures

figures: $(VENV)/.ok
	$(VPY) analysis/figures.py --results results/full --figures docs/figures \
		--html docs/results.html --numbers docs/results-numbers.json --label full

# Simulator speed on one fixed full-scale run (single thread).
bench: build
	./target/release/hpcc-sim fct --cc hpcc --workload fbhadoop --load 0.5 \
		--warmup-ms 0.5 --measure-ms 1 --tail-ms 10 --out results/bench

# Deliberate one-line bugs that the test suite must catch (~15 min).
mutants:
	$(PY) tools/mutants.py --write docs/mutants.md

clean:
	$(CARGO) clean
	rm -rf results
