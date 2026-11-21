# csv-reader-speed-01 — entry v11

[![make.fun · csv-reader-speed-01-run-20261121](https://make.fun/badge/csv-reader-speed-01-run-20261121.svg)](https://make.fun/bounty/csv-reader-speed-01-run-20261121?utm_source=github&utm_medium=readme&utm_campaign=csv-reader-speed-01-run-20261121)
[![rank](https://make.fun/badge/csv-reader-speed-01-run-20261121/jontumpil/makefun-csv-reader/rank.svg)](https://make.fun/bounty/csv-reader-speed-01-run-20261121/leaderboard?utm_source=github&utm_medium=readme&utm_campaign=csv-reader-speed-01-run-20261121)
[![reward](https://make.fun/badge/csv-reader-speed-01-run-20261121/jontumpil/makefun-csv-reader/reward.svg)](https://make.fun/bounty/csv-reader-speed-01-run-20261121?utm_source=github&utm_medium=readme&utm_campaign=csv-reader-speed-01-run-20261121)

**csv-reader 1.0.0's exact semantics, reimplemented as a Rust → WebAssembly
JSON-to-JSON transducer over the whole cases envelope, with the driver
moving the files without copying them — and a driver shell rebuilt for
the way the runs are actually timed: every run a fresh, cold process.**

For every case in `cases.json` the entry returns exactly what the reference
`readCsv(text, header)` returns — `{columns, rows, errors}` — byte-identical
to `JSON.stringify` of the reference answers, on the practice set and on
222,408 differential fuzz cases against the kit's vendor copy (token soup,
structured CSV, unicode/surrogate abuse, long texts, and whole envelopes
serialized three different ways), with zero mismatches.

## The shape of it

`solve.js` embeds a small WebAssembly module (46,232 bytes; Rust source in
`rust/`, no crates, standard library only) and exports the contract's
`run(input)`. The module does everything: it scans the `cases.json`
envelope, unescapes each case's text, runs the CSV state machine, and
writes the answers array as raw bytes. JavaScript never parses the
envelope and never sees a CSV value on the hot path.

`main.ts` stages the files through the module's own memory:

1. it asks the module for its input pointer, reads `cases.json` from disk
   straight into a view over the module's memory (no intermediate JS
   string or byte copy of a ~10 MB envelope), and calls `transduceStaged`;
2. the module returns the answers as a pointer and a length, and the
   driver writes that slice of the module's memory straight to
   `answers.json`.

If the staged path ever throws, the driver falls back to `transduceAll`
over a normally-read buffer, and then to answering case by case with
`run()` — the output is the same by construction, just slower.

## What v11 changes: the cold-run driver shell

The transducer itself is v10's, byte for byte (the same Rust source; the
rebuilt module is the same 46,232 bytes). v11's work is the shell around
it, shaped by one fact about the timing: each timed run is a brand-new
process in a brand-new container, the warm-up round happens in a
different container, and the clock reads the child process's CPU. So a
run is always cold — the module is compiled and first executed inside
the timed run, and no optimizing tier ever gets a second call to repay
its cost. Measured locally (see below), the v10 shell and the v11 shell
tie; v11 ships the configuration that cannot waste cold-run CPU:

- **Liftoff only, set before anything compiles.** `main.ts`'s first
  statements set `--liftoff-only` (and `--no-incremental-marking`, the
  condition the house baseline is timed under) through
  `setFlagsFromString`, and only then dynamically imports `solve.js`.
  No WebAssembly is compiled before the flags land, so no CPU goes to
  TurboFan compilation a single cold call can never repay. (v10 set its
  flag from inside the import path and kept a warm-up; retested
  properly — flags first, warm-up gone — Liftoff-only ties the default
  pipeline locally instead of losing, and it removes the cold-run waste
  by construction.)
- **No import-time warm-up.** v9/v10 ran a 1 KB transduction at import
  to prime the optimizing tier. With the tier pinned off and every run
  cold, the warm-up is pure overhead; it is gone.
- **Synchronous instantiation.** `solve.js` builds its module with
  `new WebAssembly.Module` / `new WebAssembly.Instance` instead of a
  top-level `await WebAssembly.instantiate(...)`: the file has no
  top-level await (so `require()` can load it too) and the module is
  compiled exactly once, at load, under the driver's flags.
- **`package.json` with `"type": "module"`.** The repo root carries it,
  so `main.ts` and `solve.js` load as the ES modules they are instead of
  paying Node's parse-as-CommonJS-then-reparse detection on every cold
  start.

## The transducer (v10's, the measured winner)

- **The envelope** is scanned, not parsed: a forgiving cursor finds each
  case's `"input"` object, unescapes its `"text"` string to WTF-8 (a
  lone surrogate becomes its 3-byte `ED A0..`/`ED B0..` form, so it
  round-trips exactly), and reads `"header"` as the JSON literal `true`,
  tolerating any key order and extra keys.
- **The reader** is a state machine over the staged bytes whose hot
  loops scan 8 bytes at a time (SWAR): one u64 load classifies quotes,
  commas, CR, LF and the bytes JSON would escape, together. Field values
  are never materialized on the hot path — plain spans copy straight
  into the output with escapes emitted inline — except the cut fields
  of a `long_row` error, which take a slower value-building reader. The
  first record is captured in a small buffer (whether it is the columns
  or row 0 is known only when it completes); every later record streams
  straight into the answer buffer, and a speculative record that turns
  out to hold no characters is rolled back by truncation.
- **The quirks are the semantics**: a leading U+FEFF is dropped; a record
  ends at LF or CRLF and a line left with no characters is no record; a
  CR outside quotes not followed by LF is dropped (`stray_cr`); a field
  opening with a quote runs to the next undoubled quote (`""` is one
  quote; CRLF and lone CR inside read as LF), what follows the closing
  quote is appended as it stands (`quote_junk`); a quote that never
  closes is a literal character and the text after it is re-read as
  unquoted (`unterminated_quote`); rows are padded (`short_row`, with
  `fields`) or cut (`long_row`, with `extra`) to the first record's
  width; errors are `{code, line, row}` in the order met.

## What v11 tried, measured interleaved in single sessions

Local harness: whole `main.ts` processes over the kit practice envelope
(10,193,953 bytes in, ~12.1 MB out), CPU time from `getrusage`
(RUSAGE_CHILDREN), net of a null control process (bare Node startup,
directory list, empty write) in the same rotation; medians of 9–11
interleaved rounds with fresh processes. This machine is throttled and
its absolute numbers drift ±20 ms between sessions; only same-session
comparisons are meaningful. In these sessions v10 nets ~113–124 ms.

First, a profile split (a walk-only build — envelope scan + unescape,
no CSV parse — against the full build, same session): the envelope
walk + unescape costs **~49 ms net**, the CSV parse + emission
**~84 ms**. Neither stage is memory-bandwidth-bound (the unescape moves
~19 MB in 49 ms); both are compute/latency-bound. That predicted the
windowing result:

- **Windowed streaming — built, correct, and rejected.** The envelope
  walk recorded each text's raw span (header known before parsing
  starts), a budget-limited unescape fed the transducer ~256 KiB of
  text at a time, and the transducer became resumable: a record that
  reaches the end of a non-final feed is unwound whole (output, errors
  and counters rolled back to the record's start) and re-read when the
  next window lands. Practice output was byte-identical — and the
  windowed build netted **~145 ms against ~111 ms** for the whole-buffer
  build in the same session, a ~30% loss: the extra span pass, the
  per-window call overhead and the re-read tail records cost far more
  than cache locality returns on a compute-bound pipeline. The
  whole-buffer architecture stays.
- **Liftoff-only, done right — kept (locally a tie).** Flags-first ESM
  driver + dynamic import + synchronous instantiation + no warm-up:
  net 120.7 ms vs v10's 118.8 in one 9-round session, 120.7 vs 117.6 in
  the final 11-round session — inside the noise both times, never a
  loss outside it. `--no-wasm-tier-up` alone tied as well (116.2).
  Kept because the tie is free and the cold-run argument is structural:
  on the timing lane there is no second call, so optimizing-tier
  compilation can only spend, never repay.
- **CommonJS driver — tied, not shipped.** A `require()`-style
  `main.ts` (relying on Node's syntax detection, `require()` of the
  now TLA-free `solve.js`) netted 116.1 — the same tie. The ESM driver
  plus `"type": "module"` says the same thing without depending on
  detection order.
- **`"type": "module"` — kept.** Micro-benchmarks (median of 15 fresh
  processes): an empty `.mjs` costs ~100 ms of this machine's CPU, an
  empty `.ts` ~155 ms — type stripping itself is ~50 ms every
  `main.ts` entry pays, control included — and declaring the module
  type removes the ~7 ms reparse slice of it.
- **Wasm diet — nothing left to strip.** The 46,232-byte module has no
  custom sections at all (no name section; the profile already strips):
  section walk shows type/function/memory/global/export/element/code/
  data only. A compile micro-benchmark (load + instantiate + one 1 KB
  transduction, median of 15 fresh processes) puts the whole
  load-and-compile at ~1 ms over bare Node startup — compilation is
  not where the time is, so there was nothing further to win here.

**Net result: v11 ties v10 locally (within ±3% in every matched
session)** and ships the shell that spends nothing on cold-run waste:
no warm-up, no optimizing tier, no reparse, one synchronous compile.
The remaining time is the transducer's honest compute — ~49 ms of
envelope + unescape and ~84 ms of parse + emission per 10 MB on this
machine — which two rounds of fusion attempts (v10) and this round's
windowing have all failed to beat.

## Correctness

- Practice set: 6/6 answers deep-identical and byte-identical to
  `JSON.stringify` of the reference answers.
- Differential fuzz vs the kit's vendor csv-reader 1.0.0: 222,408 cases,
  0 mismatches, through both `run()` and `transduceAll`, envelopes
  serialized three ways (plain, `\uXXXX`-heavy, shuffled keys with junk
  keys), including the boundary battery (escapes at span edges,
  surrogate pairs straddling spans, quotes/CR at edges, raw and escaped
  BOMs, unterminated quotes, junk after quote).
- Deterministic: two driver runs over the practice set are
  byte-identical.

## Building

`sh build.sh` rebuilds `solve.js` from `rust/` (cargo with the
`wasm32-unknown-unknown` target; `gen-solve.mjs` embeds the module as
base64 with synchronous instantiation — `node gen-solve.mjs file`
regenerates the separate-file loading instead, for comparison). The
checked-in `solve.js` is the built artifact; the entry needs nothing
built at run time.

## License

MIT (see `LICENSE`). The WebAssembly transducer is an independent
reimplementation written against csv-reader 1.0.0's documented and
observed behaviour.
