# Photobrowser build/test/run/install helper.
#
# Common usage:
#   make build
#   make run DIR=~/Pictures/2026-09-27
#   make check
#   make install                     # installs under $(HOME)/.local
#   make install PREFIX=/usr/local   # or any other prefix

PREFIX ?= $(HOME)/.local
BINDIR ?= $(PREFIX)/bin
APPDIR ?= $(PREFIX)/share/applications
DIR ?=
RAW ?=
CARGO ?= cargo

APP_ID := io.github.enricod.PhotoBrowser
DESKTOP_SRC := data/$(APP_ID).desktop
DESKTOP_DST := $(APPDIR)/$(APP_ID).desktop
BIN_DST := $(BINDIR)/photobrowser

.PHONY: help build run test test-raw check bench install uninstall clean

help:
	@echo "help       - show this list of targets"
	@echo "build      - cargo build --release"
	@echo "run        - build, then run ./target/release/photobrowser [DIR=folder]"
	@echo "test       - cargo test"
	@echo "test-raw   - cargo test against RAW samples (RAW=/path/to/raw-samples, required)"
	@echo "check      - cargo fmt --check, cargo clippy --all-targets -- -D warnings, cargo test"
	@echo "bench      - cargo bench (set RAW=/path/to/raw-samples to include RAW decode benches)"
	@echo "install    - install the binary and desktop entry under PREFIX (default: \$$HOME/.local)"
	@echo "uninstall  - remove the installed binary and desktop entry"
	@echo "clean      - cargo clean"

build:
	$(CARGO) build --release

run: build
	./target/release/photobrowser $(if $(DIR),"$(DIR)")

test:
	$(CARGO) test

test-raw:
	@if [ -z "$(RAW)" ]; then \
		echo "error: RAW is not set. Usage: make test-raw RAW=/path/to/raw-samples" >&2; \
		exit 1; \
	fi
	PB_RAW_SAMPLES="$(RAW)" $(CARGO) test

check:
	$(CARGO) fmt --check
	$(CARGO) clippy --all-targets -- -D warnings
	$(CARGO) test

bench:
	@if [ -n "$(RAW)" ]; then \
		PB_RAW_SAMPLES="$(RAW)" $(CARGO) bench; \
	else \
		$(CARGO) bench; \
	fi

install: build
	install -Dm755 target/release/photobrowser $(BIN_DST)
	install -Dm644 $(DESKTOP_SRC) $(DESKTOP_DST)
	-@command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database $(APPDIR)
	@echo "Installed binary:  $(BIN_DST)"
	@echo "Installed desktop entry: $(DESKTOP_DST)"
	@case ":$$PATH:" in \
		*":$(BINDIR):"*) ;; \
		*) echo "Note: $(BINDIR) is not on your PATH; add it to launch 'photobrowser' from a terminal." ;; \
	esac

uninstall:
	rm -f $(BIN_DST)
	rm -f $(DESKTOP_DST)
	-@command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database $(APPDIR)

clean:
	$(CARGO) clean
