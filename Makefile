.PHONY: build install test bench clean

# Where the runnable `pull-all` lives on your $PATH. Override with `make BINDIR=/some/dir`.
BINDIR ?= $(HOME)/bin

# Build the release binary, refresh the repo's bin/, and install it onto $PATH. The install is
# an atomic rename, not a plain cp: copying over a running binary fails with "Text file busy",
# and the rename is what pull-all's in-app new-build watcher keys on (the `↺ [reload]` notice).
build:
	cargo build --release
	cp target/release/pull-all bin/pull-all
	@mkdir -p $(BINDIR)
	cp target/release/pull-all $(BINDIR)/pull-all.new
	mv -f $(BINDIR)/pull-all.new $(BINDIR)/pull-all

# `build` already installs the main binary; this adds the sibling backends (go/bun/bash).
install: build
	mkdir -p $(BINDIR)/pull-all-siblings
	cp pull-all-siblings/pull-all-repos $(BINDIR)/pull-all-siblings/pull-all-repos

test:
	cargo test

bench:
	@echo "Running benchmark on current directory (use --timeout 5 for quick mode)..."
	time bin/pull-all --no-tui 2>&1

clean:
	cargo clean
	rm -f bin/pull-all
