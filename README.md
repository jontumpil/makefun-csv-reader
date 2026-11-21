# csv-reader-speed-01 — entry v10

[![make.fun · csv-reader-speed-01-run-20261121](https://make.fun/badge/csv-reader-speed-01-run-20261121.svg)](https://make.fun/bounty/csv-reader-speed-01-run-20261121?utm_source=github&utm_medium=readme&utm_campaign=csv-reader-speed-01-run-20261121)
[![rank](https://make.fun/badge/csv-reader-speed-01-run-20261121/jontumpil/makefun-csv-reader/rank.svg)](https://make.fun/bounty/csv-reader-speed-01-run-20261121/leaderboard?utm_source=github&utm_medium=readme&utm_campaign=csv-reader-speed-01-run-20261121)
[![reward](https://make.fun/badge/csv-reader-speed-01-run-20261121/jontumpil/makefun-csv-reader/reward.svg)](https://make.fun/bounty/csv-reader-speed-01-run-20261121?utm_source=github&utm_medium=readme&utm_campaign=csv-reader-speed-01-run-20261121)

**csv-reader 1.0.0's exact semantics, reimplemented as a Rust → WebAssembly
JSON-to-JSON transducer over the whole cases envelope, with the driver
moving the files without copying them.**

For every case in `cases.json` the entry returns exactly what the reference
`readCsv(text, header)` returns — `{columns, rows, errors}` — byte-identical
to `JSON.stringify` of the reference answers, on the practice set and on
222,408 differential fuzz cases against the kit's vendor copy (token soup,
structured CSV, unicode/surrogate abuse, long texts, and whole envelopes
serialized three different ways), with zero mismatches.

## The shape of it

`solve.js` embeds a small WebAssembly module (Rust source in `rust/`,
no crates, standard library only) and exports the contract's `run(input)`.
The module does everything: it scans the `cases.json` envelope, unescapes
each case's text, runs the CSV state machine, and writes the answers array
as raw bytes. JavaScript never parses the envelope and never sees a CSV
value on the hot path.

`main.ts` is the same driver shape as the house baseline's, and stages the
files through the module's own memory:

1. it asks the module for its input pointer, reads `cases.json` from disk
   straight into a view over the module's memory (no intermediate JS
   string or byte copy of an 11 MB envelope), and calls `transduceStaged`;
2. the module returns the answers as a pointer and a length, and the
   driver writes that slice of the module's memory straight to
   `answers.json`.

One fallback is kept: if the input file is larger than the memory the
module can address, the driver reads it the ordinary way and calls
`transduceAll` instead; a case the transducer cannot answer (none is
known) falls back to a JavaScript mirror of the reference reader, kept
verbatim from the previous versions of this entry.

## The transducer (unchanged from v9 — it is the measured winner)

- **The envelope** is scanned, not parsed: a forgiving cursor finds each
  case's `"input"` object, its `"text"` string span and its `"header"`
  literal, tolerating any key order and extra keys. The text string is
  unescaped into a WTF-8 staging buffer (a lone surrogate becomes its
  3-byte `ED A0..`/`ED B0..` form, so it round-trips exactly).
- **The reader** is a byte-at-a-time state machine over the staged bytes
  whose hot loops scan 8 bytes at a time (SWAR): one u64 load classifies
  quotes, commas, CR and LF together. Plain fields copy out as spans;
  quoted fields likewise between event bytes; a field with no event byte
  at all is emitted from a single span. Values stream into the output
  buffer as JSON — string escaping is decided 8 bytes at a time with a
  256-entry class table — and the row/column width accounting runs on
  counts taken during the scan, not on a second pass.
- **The quirks are the semantics**: the leading-BOM strip (one code point,
  before anything else, so a BOM inside the first quoted field still
  counts), the dropped stray CR (and the astral pair that materializes
  when the CR stood between two lone surrogates), junk after a closing
  quote being swallowed to the next delimiter, rows padded with `""` to
  the first row's width while `columns` follows the header or the first
  row, and the exact error objects `{type, code, message, row}` —
  `Quotes` ("Quoted field unterminated"), `FieldMismatch`
  ("Too few fields: expected %d fields but parsed %d") with its
  first-row-suppressed variant, and `Input` for a `null` text.
- **One warm-up transduction** of a tiny representative envelope runs at
  import time, so the module's hot functions are compiled before the
  timed call rather than during it.

## What v10 tried, measured interleaved against v9 in one session

Local harness: whole `main.ts` processes over the practice envelope
(11,064,623 bytes in, 12,073,155 out), CPU time from `getrusage`, net of
an empty house-style control process run in the same rotation; medians
of 7–13 interleaved rounds. Absolute numbers drift on this machine;
only same-session comparisons are meaningful. v9 nets ~110–125 ms in
these sessions.

- **Read-side escape fusion — rejected.** The staging pass was removed:
  the envelope walk records each text's raw span and a fused reader
  consumes field content as bulk envelope spans wherever the span holds
  no backslash, decoding escapes unit-by-unit only around actual escape
  sequences, with v9's emission and accounting rules reused exactly
  (span/escape event logic, pending-high surrogate handling, WTF-8
  counting). It was correct — practice byte-identical — but slower in
  both shapes tried: as a second reader beside the staged one (net
  ~141 ms vs v9's ~115; the module grew 52 KB → 81 KB and import tripled)
  and as the only reader (~135–153 ms vs ~114–123 across batches).
  Removing the import warm-up made it worse still. The unescape pass is
  memcpy-cheap; the fused machine pays per-escape dispatch and a larger
  code footprint in the tier-up-critical first call. The staged design
  stays.
- **Separate `solve.wasm` file — measured neutral, not shipped.**
  Loading via `readFileSync(new URL("./solve.wasm", import.meta.url))` +
  `WebAssembly.instantiate` tied the embedded base64 loading in every
  interleaved batch (import ~4.5 ms either way; nets within ±4 ms), and
  the pair was verified working from a clean directory containing only
  the shipped files. Synchronous `WebAssembly.Module`/`Instance`
  construction tied as well. Because the gain is nil and an embedded
  `solve.js` stays self-contained wherever the grader copies it, v10
  keeps the v9 loading.
- **Optimization level — "s" kept, "z" rejected.** `opt-level = "s"`
  shrinks the module 52,387 → 46,232 bytes and was the most consistent
  small win of the round (nets ~105–110 vs v9's ~110–125 in matched
  batches). `opt-level = "z"` shrinks it to 32,428 bytes but gives the
  hot loops back (net ~120). `opt-level = 3` ties 2, as v9 found.
- **Driver trims — nothing measurable, two rejected.** Skipping the
  input buffer's zero-fill in `in_ptr` (reserve + `set_len`) tied the
  zero-filled version and was reverted — fresh pages cost the same
  faulted once either way. `--no-wasm-tier-up` and `--liftoff-only`
  (via `setFlagsFromString` in `main.ts`) both lost by ~4–6 ms: even
  for a single cold call, tier-up pays. The rest of the driver's cost
  is Node's own startup, which the control process pays too.

**Net result: v10 ≈ 5–8% faster than v9 interleaved** (final matched
batch, 9 rounds: v9 net 114.6 ms, v10 net 109.3 ms) — a small, real
improvement, honestly short of the 20% hoped for: after v9, what
remains is roughly half warm compute in a reader that is already
span-based, and half first-call/process overhead that resisted the
warm-up, size and flag attacks tried across both versions.

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
base64 — `node gen-solve.mjs file` regenerates the separate-file
loading instead, for comparison). The checked-in `solve.js` is the
built artifact; the entry needs nothing built at run time.

## License

MIT (see `LICENSE`). The JavaScript fallback reader in `solve.js` is
this project's own earlier work; the WebAssembly transducer is an
independent reimplementation written against csv-reader 1.0.0's
documented and observed behaviour.
