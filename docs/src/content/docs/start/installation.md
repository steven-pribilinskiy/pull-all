---
title: Installation
description: Install polygit with the one-line script, with cargo, or build from source.
---

`polygit` is a single Rust binary for **Linux and macOS** (on Windows, run it under WSL).

## Install script

The quickest way — downloads the right prebuilt binary for your platform:

```bash
curl -fsSL https://steven-pribilinskiy.github.io/polygit/install.sh | bash
```

It installs to `~/.local/bin/polygit` by default (override with `POLYGIT_INSTALL=/some/dir`) and
tells you if that directory isn't on your `PATH`. Re-run it any time to update.

## With cargo

If you have the Rust toolchain ([rustup](https://rustup.rs)), install straight from the repo —
no clone needed:

```bash
cargo install --git https://github.com/steven-pribilinskiy/polygit
```

This builds the latest `main` and drops `polygit` in `~/.cargo/bin`.

## From source

To hack on it, or to get the bash/Go/Bun sibling backends too:

```bash
git clone https://github.com/steven-pribilinskiy/polygit.git
cd polygit
make install      # release build → ~/bin/polygit (+ the bash sibling)
```

`make build` just builds + installs the binary; `make install` also drops the `polygit-repos`
bash backend into `~/bin/polygit-siblings/`. Make sure the target dir is on your `PATH`.

## Verify

```bash
polygit --version
```

## Next steps

- [Usage](../usage/) — run it, pass flags, and read the panes.
- [Keybindings](../../guides/keybindings/) — drive it from the keyboard.
