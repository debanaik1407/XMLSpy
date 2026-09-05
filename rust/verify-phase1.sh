#!/usr/bin/env bash
# Phase 1 verification — run this on a machine that has the Rust toolchain.
#
# The Phase 1 engine work (mmap, parallel index builder, write-ahead journal + recovery,
# rope edit buffer, folding/brackets/bookmarks, the conformance runner) was written in a
# sandbox with no Rust toolchain and no network, so none of it has been compiled. This
# script is the one command that turns "written" into "verified": it builds, lints, tests
# and benchmarks everything Phase 1 promises, prints a PASS/FAIL table, and leaves the
# numbers you should paste into bench/reports/.
#
#   ./verify-phase1.sh                 # everything, 64 MiB corpus
#   CORPUS=/path/big.xml ./verify-phase1.sh
#   SIZE=1GiB THREADS=8 ./verify-phase1.sh
#   XMLCONF=/path/to/xmlconf/xmltest ./verify-phase1.sh   # + the real W3C suite
#
# Exit code: 0 when every gate passed, 1 otherwise. Nothing here mutates the repository
# except the generated corpus (in $TMPDIR) and the .xsi cache (in $CACHE_DIR).

set -uo pipefail
cd "$(dirname "$0")"

SIZE="${SIZE:-64MiB}"
THREADS="${THREADS:-8}"
CORPUS="${CORPUS:-}"
XMLCONF="${XMLCONF:-}"
CACHE_DIR="${CACHE_DIR:-${TMPDIR:-/tmp}/xmlspy-verify-cache}"
WORK="${WORK:-${TMPDIR:-/tmp}/xmlspy-verify}"
LOG="$WORK/log.txt"
mkdir -p "$WORK" "$CACHE_DIR"
: > "$LOG"

PASS=0
FAIL=0
declare -a RESULTS=()

# ---------------------------------------------------------------- helpers

step() {
  local name="$1"; shift
  echo ""
  echo "=================================================================="
  echo "== $name"
  echo "=================================================================="
  echo "\$ $*" | tee -a "$LOG"
  if "$@" >>"$LOG" 2>&1; then
    echo "   PASS"
    RESULTS+=("PASS  $name")
    PASS=$((PASS + 1))
    return 0
  else
    local rc=$?
    echo "   FAIL (exit $rc) — see $LOG"
    RESULTS+=("FAIL  $name")
    FAIL=$((FAIL + 1))
    return 1
  fi
}

# A step whose *output* is the verdict (benchmarks, conformance percentages).
step_out() {
  local name="$1"; shift
  echo ""
  echo "------------------------------------------------------------------"
  echo "-- $name"
  echo "-- \$ $*"
  "$@" 2>&1 | tee -a "$LOG"
}

gate() {
  local name="$1" ok="$2" detail="$3"
  if [ "$ok" = "1" ]; then
    RESULTS+=("PASS  $name — $detail")
    PASS=$((PASS + 1))
  else
    RESULTS+=("FAIL  $name — $detail")
    FAIL=$((FAIL + 1))
  fi
}

# ---------------------------------------------------------------- 0. toolchain

echo "XMLSpy-rs Phase 1 verification — $(date -u '+%Y-%m-%d %H:%M UTC')" | tee "$LOG"
echo "host: $(uname -srm) · cpus: $(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo '?')" | tee -a "$LOG"

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo not found. Install the pinned toolchain (rust-toolchain.toml) and re-run." | tee -a "$LOG"
  exit 2
fi
cargo --version | tee -a "$LOG"
rustc --version | tee -a "$LOG"

# ---------------------------------------------------------------- 1. build & lint

step "cargo fmt --check"        cargo fmt --all -- --check
step "cargo clippy -D warnings" cargo clippy --workspace --all-targets -- -D warnings
step "cargo build --release"    cargo build --release --workspace
step "cargo test --workspace"   cargo test --workspace

XMLSPY="$PWD/target/release/xmlspy"
if [ ! -x "$XMLSPY" ]; then
  echo "no xmlspy binary at $XMLSPY — cannot continue" | tee -a "$LOG"
  exit 2
fi

# ---------------------------------------------------------------- 2. the Phase 1 test suites, named

step "parity: parallel == sequential"    cargo test -p xmlspy-parallel --release
step "rope: property suite vs oracle"    cargo test -p xmlspy-rope --release
step "io: mmap, cache, journal"          cargo test -p xmlspy-io --release
step "parse: resumable + boundaries"     cargo test -p xmlspy-parse --release
step "index: .xsi, folding, bookmarks"   cargo test -p xmlspy-index --release
step "cli: conformance suite as a gate"  cargo test -p xmlspy-cli --release

# ---------------------------------------------------------------- 3. corpus

if [ -z "$CORPUS" ]; then
  CORPUS="$WORK/corpus-$SIZE.xml"
  if [ ! -s "$CORPUS" ]; then
    step "generate $SIZE corpus" "$XMLSPY" gen --size "$SIZE" --out "$CORPUS"
  else
    echo "reusing $CORPUS" | tee -a "$LOG"
  fi
fi
ls -l "$CORPUS" | tee -a "$LOG"

# ---------------------------------------------------------------- 4. gates

step_out "bench: 1 thread (sequential)" \
  "$XMLSPY" bench "$CORPUS" --runs 3 --sequential --cache-dir "$CACHE_DIR"
step_out "bench: $THREADS threads (parallel)" \
  "$XMLSPY" bench "$CORPUS" --runs 3 --threads "$THREADS" --min-segment 1MiB --cache-dir "$CACHE_DIR"

BENCH_1T=$("$XMLSPY" bench "$CORPUS" --runs 1 --sequential --cache-dir "$CACHE_DIR" 2>/dev/null \
  | awk '/gate index >= 500 MB\/s single-thread/ {print $0}')
BENCH_NT=$("$XMLSPY" bench "$CORPUS" --runs 1 --threads "$THREADS" --min-segment 1MiB --cache-dir "$CACHE_DIR" 2>/dev/null \
  | awk '/gate index >= 1.2 GB\/s/ {print $0}')
RSS=$("$XMLSPY" index "$CORPUS" --out "$WORK/corpus.xsi" --cache-dir "$CACHE_DIR" 2>/dev/null \
  | awk '/peak RSS/ {print $3, $4}')

case "$BENCH_1T" in *"PASS"*) gate "≥ 500 MB/s, 1 thread" 1 "$BENCH_1T";;
                            *) gate "≥ 500 MB/s, 1 thread" 0 "${BENCH_1T:-no output}";; esac
case "$BENCH_NT" in *"PASS"*) gate "≥ 1.2 GB/s, $THREADS threads" 1 "$BENCH_NT";;
                            *) gate "≥ 1.2 GB/s, $THREADS threads" 0 "${BENCH_NT:-no output}";; esac
echo "   RSS on $SIZE: ${RSS:-unknown} (gate: < 512 MiB)"

# first paint proxy: time from cold cache to "wf" verdict on the corpus
step_out "first paint proxy (wf, cold)" \
  "$XMLSPY" wf "$CORPUS" --threads "$THREADS" --min-segment 1MiB --max-errors 5

# ---------------------------------------------------------------- 5. features

step_out "edit: rope, one 3-byte edit" bash -c \
  "'$XMLSPY' edit '$CORPUS' --insert-after 3 --text '  <!-- x -->' --show --out '$WORK/edited.xml'"
step_out "fold: regions in the first 2000 lines" bash -c \
  "'$XMLSPY' fold '$CORPUS' --lines 1-2000"
step_out "fold: bracket match at offset 0" bash -c "'$XMLSPY' fold '$CORPUS' --bracket 0"
step "index cache: second open is a hit" bash -c \
  "'$XMLSPY' index '$CORPUS' --cache-dir '$CACHE_DIR' | grep -q 'cache hit'"
step_out "recover: no journal left after a clean build" bash -c \
  "'$XMLSPY' recover '$CORPUS'; test \$? -eq 1"

# ---------------------------------------------------------------- 6. conformance

# The suite itself is checkable without Rust: this drives the same 41 cases through the
# TypeScript reference engine (41/41 as of 2026-09-05). Skipped when the app's node_modules
# is not installed.
if [ -d ../XMLSpy/node_modules ]; then
  step "conformance suite vs the TS reference (npm)"     npm --prefix ../XMLSpy run test:conformance
else
  RESULTS+=("SKIP  conformance suite vs the TS reference — run npm ci in ../XMLSpy first")
fi

step_out "conformance: vendored mini suite" "$XMLSPY" conformance
if "$XMLSPY" conformance >/dev/null 2>&1; then
  gate "mini conformance suite at 100 %" 1 "$("$XMLSPY" conformance | tail -1)"
else
  gate "mini conformance suite at 100 %" 0 "$("$XMLSPY" conformance | tail -1)"
fi

if [ -n "$XMLCONF" ] && [ -d "$XMLCONF" ]; then
  step_out "conformance: W3C xmlconf at $XMLCONF" "$XMLSPY" conformance --suite "$XMLCONF"
  if "$XMLSPY" conformance --suite "$XMLCONF" >/dev/null 2>&1; then
    gate "W3C not-wf conformance at 100 %" 1 "$("$XMLSPY" conformance --suite "$XMLCONF" | tail -1)"
  else
    gate "W3C not-wf conformance at 100 %" 0 "$("$XMLSPY" conformance --suite "$XMLCONF" | tail -1)"
  fi
else
  RESULTS+=("SKIP  W3C not-wf conformance — set XMLCONF=/path/to/xmlconf/xmltest")
fi

# ---------------------------------------------------------------- summary

echo ""
echo "=================================================================="
echo "== Phase 1 verification summary"
echo "=================================================================="
for r in "${RESULTS[@]}"; do echo "  $r"; done
echo ""
echo "  $PASS passed, $FAIL failed · log: $LOG"
echo ""
echo "Paste the numbers above into bench/reports/<date>-<host>.md, then tick the"
echo "matching boxes in TODO.md §4 Phase 1 (they are marked 'unverified in sandbox')."
[ "$FAIL" -eq 0 ]
