# Build, test and install the compiler and its runtime library.
#
#   make                          build the compiler and runtime, optimised
#   make run FILE=app.wb          compile a program and run it
#   make install                  install the `weby` command into ~/.local (PREFIX=... to change)
#   make link                     the same, but the command follows this checkout's builds
#   make help                     list every target
#
# The packages to install first are listed in docs/building.md; `make check-deps` checks for them.

# The command's name. It is also set in crates/lang_cli/Cargo.toml and crates/lang_names; see
# "Renaming the language" in docs/03-compiler-architecture.md.
CLI := weby

CARGO      ?= cargo
# Extra arguments for every cargo command, for example CARGOFLAGS=--offline.
CARGOFLAGS ?=
PYTHON     ?= python3
PREFIX     ?= $(HOME)/.local
BINDIR     ?= $(PREFIX)/bin
LIBDIR     ?= $(PREFIX)/lib/$(CLI)
# Which build `make build`, `run`, `program` and `install` use. `release` optimises the compiler
# and the runtime library that every program links, and compiles programs with --release.
PROFILE    ?= release

ifeq ($(PROFILE),release)
  RELEASE_FLAG := --release
else ifeq ($(PROFILE),debug)
  RELEASE_FLAG :=
else
  $(error PROFILE must be release or debug, not '$(PROFILE)')
endif

COMPILER := target/$(PROFILE)/$(CLI)
RUNTIME  := target/$(PROFILE)/liblang_rt.a

.DEFAULT_GOAL := build
.PHONY: build release debug run program test stress lint clippy fmt fmt-check check-deps \
	install link uninstall bench bench-http bench-ws clean help

## Building

build: ## Build the compiler and runtime library (PROFILE=release, or PROFILE=debug)
	$(CARGO) build --workspace $(RELEASE_FLAG) $(CARGOFLAGS)

release: ## Build the optimised compiler and runtime into target/release
	$(CARGO) build --workspace --release $(CARGOFLAGS)

debug: ## Build the unoptimised compiler and runtime into target/debug (quicker to build)
	$(CARGO) build --workspace $(CARGOFLAGS)

## Programs

run: build ## Compile a program and run it: make run FILE=app.wb [ARGS="arguments"]
	@test -n "$(FILE)" || { echo "usage: make run FILE=path/to/app.wb [ARGS=\"...\"]" >&2; exit 2; }
	$(COMPILER) run $(RELEASE_FLAG) $(FILE) $(ARGS)

program: build ## Compile a program to an executable: make program FILE=app.wb [OUT=app]
	@test -n "$(FILE)" || { echo "usage: make program FILE=path/to/app.wb [OUT=path]" >&2; exit 2; }
	$(COMPILER) build $(RELEASE_FLAG) $(FILE) $(if $(OUT),-o $(OUT))

## Installing

install: build ## Build, then install the `weby` command into PREFIX (default ~/.local)
	install -d $(DESTDIR)$(BINDIR) $(DESTDIR)$(LIBDIR)
	install -m 755 $(COMPILER) $(DESTDIR)$(LIBDIR)/$(CLI)
	install -m 644 $(RUNTIME) $(DESTDIR)$(LIBDIR)/liblang_rt.a
	ln -sf $(LIBDIR)/$(CLI) $(DESTDIR)$(BINDIR)/$(CLI)
	@$(path-hint)

link: build ## Build, then make `weby` run this checkout's build, so every rebuild updates it
	install -d $(BINDIR)
	ln -sf $(CURDIR)/$(COMPILER) $(BINDIR)/$(CLI)
	@$(path-hint)

uninstall: ## Remove what `make install` or `make link` put in PREFIX
	rm -f $(DESTDIR)$(BINDIR)/$(CLI) $(DESTDIR)$(LIBDIR)/$(CLI) $(DESTDIR)$(LIBDIR)/liblang_rt.a
	@rmdir $(DESTDIR)$(LIBDIR) 2>/dev/null || true

# After installing: say how to run the command, or how to put BINDIR on the PATH if it is not.
define path-hint
case ":$$PATH:" in \
	*":$(BINDIR):"*) echo "Installed. Run: $(CLI) app.wb -o app" ;; \
	*) echo "Installed $(BINDIR)/$(CLI), but $(BINDIR) is not on your PATH. Add this line to ~/.bashrc or ~/.profile:"; \
	   echo "    export PATH=\"$(BINDIR):\$$PATH\"" ;; \
esac
endef

## Checking

test: ## Run every test: unit tests, the end-to-end programs, the guide, the HTTP servers
	$(CARGO) test --workspace $(CARGOFLAGS)

stress: release ## Run the concurrency programs many times: make stress [RUNS=1000]
	sh tests/stress.sh $(RUNS)

lint: clippy fmt-check ## Run clippy and check the Rust formatting

clippy: ## Run clippy on everything, with warnings as errors
	$(CARGO) clippy --workspace --all-targets $(CARGOFLAGS) -- -D warnings

fmt: ## Format the Rust sources
	$(CARGO) fmt --all

fmt-check: ## Check the Rust formatting without changing anything
	$(CARGO) fmt --all --check

check-deps: ## Check that the required tools and libraries are installed
	@sh scripts/check-deps.sh

## Benchmarks (bench/README.md)

bench: release ## Run the language benchmarks against Go and CPython: make bench [ARGS="--runs 3"]
	$(PYTHON) bench/run.py $(ARGS)

bench-http: release ## Run the HTTP server benchmark against Go and Python (needs Go)
	$(PYTHON) bench/http/run.py $(ARGS)

bench-ws: release ## Measure the memory of idle WebSocket connections
	$(PYTHON) bench/ws_idle/run.py $(ARGS)

## Other

clean: ## Remove everything cargo built
	$(CARGO) clean

help: ## List the targets
	@awk 'BEGIN { FS = ":.*## " } \
		/^## / { printf "\n%s\n", substr($$0, 4) } \
		/^[a-z-]+:.*## / { printf "  %-12s %s\n", $$1, $$2 }' $(MAKEFILE_LIST)
	@echo
	@echo "Variables: PROFILE=$(PROFILE) PREFIX=$(PREFIX) CARGOFLAGS=$(CARGOFLAGS)"
