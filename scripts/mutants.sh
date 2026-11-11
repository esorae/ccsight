#!/usr/bin/env bash
# Mutation testing: break one line at a time and see whether any test fails.
# A mutant that survives marks code the tests run but never check. Manual, not
# a gate — a run takes minutes. Exits as cargo-mutants does (2: a mutant
# survived, 3: one timed out); 1 for this script's own errors. Usage:
#   scripts/mutants.sh                      lines changed since origin/main
#   scripts/mutants.sh diff <base> [<rev>]  since <base>, in the work tree or <rev>
#   scripts/mutants.sh file <path>...       every mutant in those files
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

if ! command -v cargo-mutants >/dev/null 2>&1; then
    echo "cargo-mutants is not installed: cargo install --locked cargo-mutants" >&2
    exit 1
fi

mode="${1:-diff}"
target=()
case "$mode" in
    diff)
        base="${2:-origin/main}"
        diff_file="$(mktemp)"
        git diff "$base" ${3:+"$3"} > "$diff_file"
        target=(--in-diff "$diff_file")
        ;;
    file)
        shift
        [ "$#" -gt 0 ] || { echo "file: name at least one path" >&2; exit 1; }
        for f in "$@"; do target+=(--file "$f"); done
        ;;
    *)
        echo "usage: scripts/mutants.sh [diff [base [rev]] | file <path>...]" >&2
        exit 1
        ;;
esac

# A mutant can break the guard that keeps tests off the real ~/.ccsight, so
# the run gets a throwaway HOME; the toolchain stays where it is.
scratch_home="$(mktemp -d)"
trap 'rm -rf "$scratch_home" "${diff_file:-}"' EXIT

mkdir -p target
cargo_home="${CARGO_HOME:-$HOME/.cargo}"
rustup_home="${RUSTUP_HOME:-$HOME/.rustup}"

# Two jobs: more builds at once contend for CPU and finish later.
status=0
HOME="$scratch_home" \
    CARGO_HOME="$cargo_home" \
    RUSTUP_HOME="$rustup_home" \
    CCSIGHT_NO_HOOK_INSTALL=1 \
    cargo mutants -j 2 "${target[@]}" --output target/mutants || status=$?

missed=target/mutants/mutants.out/missed.txt
if [ -s "$missed" ]; then
    echo
    echo "Survived (add a check, or list an equivalent mutant in .cargo/mutants.toml):"
    cat "$missed"
fi
exit "$status"
