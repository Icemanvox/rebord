#!/bin/sh
# Compilation croisée depuis Linux vers Windows x64 (MinGW), bibliothèque standard recompilée.
# Prérequis : rustc stable, sources de la std (rust-src), mingw-w64.
set -e
cd "$(dirname "$0")/.."
RUSTC_BOOTSTRAP=1 cargo build --release --target x86_64-pc-windows-gnu \
  -Zbuild-std=std,panic_unwind "$@"
ls -l target/x86_64-pc-windows-gnu/release/rebord.exe
