# csv-reader, read faster (v13) — an exact drop-in for csv-reader 1.0.0, as a Rust → WebAssembly transducer

[![make.fun · csv-reader-speed-01-run-20261121](https://make.fun/badge/csv-reader-speed-01-run-20261121.svg)](https://make.fun/bounty/csv-reader-speed-01-run-20261121?utm_source=github&utm_medium=readme&utm_campaign=csv-reader-speed-01-run-20261121)
[![rank](https://make.fun/badge/csv-reader-speed-01-run-20261121/jontumpil/makefun-csv-reader/rank.svg)](https://make.fun/bounty/csv-reader-speed-01-run-20261121/leaderboard?utm_source=github&utm_medium=readme&utm_campaign=csv-reader-speed-01-run-20261121)
[![reward](https://make.fun/badge/csv-reader-speed-01-run-20261121/jontumpil/makefun-csv-reader/reward.svg)](https://make.fun/bounty/csv-reader-speed-01-run-20261121?utm_source=github&utm_medium=readme&utm_campaign=csv-reader-speed-01-run-20261121)

An entry for make.fun's [csv-reader-speed-01-run-20261121](https://make.fun/bounty/csv-reader-speed-01-run-20261121)
bounty: write a faster drop-in for make.fun's own csv-reader 1.0.0 — same answer on every
input, less CPU time. Places go by CPU time alone, and an entry that misses the reference's
answer on even one case is not timed at all.

This is the thirteenth revision of the entry. It keeps v9's architecture — a single
Rust → WebAssembly JSON-to-JSON transducer with SWAR word-at-a-time scanners, zero-copy
staging across the JS↔WASM boundary, and streaming emission — and changes the build, not
the algorithm: the Rust crate is compiled with **opt-level "s"**, which interleaved
measurement showed beats both opt-level 2 (v9) and opt-level 3 for this code on the
practice set, and the smaller module (46 KB of wasm) also loads cheaper. A
`package.json` (`{"type": "module"}`) is shipped so Node parses the entry's modules as
ES modules directly instead of reparsing them.

## What it does

`solve.js` exports `run(input)`, which reads one case's `{text, header}` and returns
`{columns, rows, errors}` under csv-reader 1.0.0's broken-row policy, exactly:

- a leading U+FEFF is dropped; records end at LF or CRLF; a line with no characters is no
  record;
- a stray CR outside quotes is dropped and reported (`stray_cr`);
- quoted fields undouble `""` to `"`, read CRLF and lone CR inside quotes as LF, append
  whatever follows the closing quote as it stands (`quote_junk`), and a quote that never
  closes becomes a literal character whose tail is re-read as an unquoted field
  (`unterminated_quote`);
- rows are padded or cut to the first record's width (`short_row` / `long_row`), and every
  error carries the line its record starts on and its index in `rows` (null for the header).

`main.ts` is the harness driver: it reads `/input/cases.json` **as raw bytes** and hands
them to the transducer whole — the answers come back as raw bytes too, so the practice
set's ~11 MB envelope is never `JSON.parse`d or `JSON.stringify`d in JavaScript at all.
(If the transducer ever threw, the driver falls back first to `transduceAll`, then to
parsing and answering case by case with `run()`, so the output stays correct.)

## How it is fast

The whole reader is a Rust crate compiled to WebAssembly, embedded in `solve.js` as
base64 (46 KB of wasm). Inside the module, one pass turns envelope bytes into answer
bytes:

- **The envelope is scanned in Rust.** A forgiving JSON scanner finds each case's `input`
  (any key order, extra keys skipped), and each `text` string is unescaped straight to
  WTF-8 — lone surrogates keep their 3-byte `ED A0..`/`ED B0..` encodings, so they
  round-trip exactly instead of being replaced the way `TextEncoder` would replace them.
  `"\r\n"` escape pairs — every CRLF line ending — are taken in one step.
- **The CSV reader emits JSON directly.** A field's value is never materialized on the hot
  path: the reader scans a word at a time (SWAR: eight bytes per u64, exact per-lane
  masks for the structural bytes and for every byte `JSON.stringify` would escape), copies
  each plain span of a field straight into the output buffer through a raw write cursor,
  and writes escapes inline. Speculative output is rolled back by truncation — a record
  that turns out to hold no characters, and a quote that never closes, both unwind that
  way. Only the cut fields of a `long_row` error, which the answer must carry as values,
  take a slower value-building reader.
- **Emission is byte-identical to `JSON.stringify` of the reference answer** — same key
  order, same short escapes, `\u00xx` lowercase for the other C0 controls, `\udXXX` for a
  lone surrogate. One subtlety that needed care: when a dropped stray CR (or a closing
  quote followed by junk) leaves a high surrogate directly before a low one in the answer
  string, `JSON.stringify` emits the pair as the raw astral character. The emitter holds a
  high surrogate back until the field's next value character is known, so the pair forms
  exactly when it should.
- **Zero-copy staging.** `main.ts` `readSync`s `cases.json` directly into a view over the
  module's input buffer (no copy in) and `writeFileSync`s the answer straight from a view
  over the module's output buffer (no copy out).
- **Streaming emission.** Only the first record is captured in a small buffer (whether it
  is the columns array or row 0 is known only once it completes); every later record
  streams straight into the answer buffer.
- **A 1 KB warm-up envelope runs at import time**, so the runtime's first-use compilation
  of the module is paid on a toy input instead of inside the first real case.

### What v13 changes over v9 (and why not v12)

- **opt-level "s"** for the Rust crate (wasm 52 KB → 46 KB). Measured interleaved on one
  pinned CPU against opt-level 2 and opt-level 3 builds of the identical source: "s" is
  consistently the fastest of the three here (~8–10 ms of net on the practice set).
- **`package.json` with `"type": "module"`** (as v12 shipped), removing Node's
  parse-as-CommonJS-then-reparse step for `main.ts` and `solve.js`.
- v12's dual-module SWAR+SIMD128 dispatch is **not** carried over: on the board it scored
  behind the single-module builds, and locally its packaging cost is real. One module,
  SWAR only.

### Measured, and rejected (this round)

- **opt-level 3**: ties opt-level 2 locally, loses to "s". Rejected.
- **Larger initial `out.reserve`** in `transduce` (input length + ⅛): no measurable gain
  on top of "s". Rejected.
- **`--liftoff-only`** driver (pin wasm compilation to the baseline tier): a tie with
  default tiering under a pinned single CPU locally. Rejected — default tiering is the
  configuration the earlier scored revisions ran.

## Measured

Locally (this machine, Node 24, process CPU by rusage, runs pinned to one CPU and
interleaved in rotating order, median of 13, on the kit's practice set; the grading host
differs — the ratios are the stable part):

| entry | gross | net (minus control) |
| --- | ---: | ---: |
| null-entry control (lists input dir, writes `[]`) | 157 ms | — |
| v8 (Rust → WASM, copying shell) | 315 ms | 158 ms |
| v12 (dual-module SWAR+SIMD128) | 316 ms | 159 ms |
| v9 (single module, opt-level 2) | 278 ms | 122 ms |
| **v13 (this entry)** | **266 ms** | **109 ms** |

So v13 is about **1.45× faster than v8** net on this machine. A phase profile of the v8
shell attributes its net roughly as: ~17 ms module load + envelope read, ~141 ms
transduce + copy out, ~22 ms writing the 12 MB answer file; v13's wins come from removing
the copies (v9's staging/streaming) and from the "s" build.

## Verification

- Practice set: the kit's own scorer passes **6/6 cases** against the practice reference,
  and the produced `answers.json` is **byte-identical** to v8/v9/v12's (SHA-256
  `bb5b6cc5c523edf0b18a20cd1f577b0d461b3ae021fd3ab2c9b4e4a5f01002ed`).
- Differential fuzzing against the reference implementation (the kit baseline's vendored
  csv-reader), through both `run()` per case and `transduceAll` over envelopes serialized
  three ways (plain, `\uXXXX`-heavy, shuffled keys with extra junk keys), each envelope
  also byte-compared: **1,333,816 cases, 0 mismatches** across two seeds — covering quotes
  and doubled quotes at field and record edges, unterminated quotes, junk after closing
  quotes, CRLF vs lone CR inside and outside quotes, quote pairs straddling spans, empty
  lines, BOM (raw and escaped), short/long rows, header on/off, unicode, emoji, lone
  surrogates, envelope escapes including surrogate pairs, empty texts, and texts of only
  newlines, quotes, or commas.
- Determinism: repeated runs over the practice input produce byte-identical output.

## Layout

- `solve.js` — the entry module: the embedded wasm plus the JS glue (`run`,
  `transduceAll`, and the staged zero-copy API `main.ts` uses). Dependency-free;
  Node 22 standard library only.
- `main.ts` — the harness driver (turns V8's incremental marking off, stages the
  envelope bytes directly into the module, writes the answer bytes from the module;
  fallbacks if the transducer throws).
- `package.json` — `{"type": "module"}`.
- `rust/` — the Rust source (`Cargo.toml`, `src/lib.rs`); `build.sh` rebuilds the wasm
  and regenerates `solve.js` from `solve.template.js` via `gen-solve.mjs`. The submitted
  `solve.js` already contains the built wasm; the source is for provenance.
- `LICENSE` — MIT.

Total repository size is about 90 KB, far under the competition's 1 MiB cap.
