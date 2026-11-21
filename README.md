# csv-reader-speed-01 — entry v12

[![make.fun · csv-reader-speed-01-run-20261121](https://make.fun/badge/csv-reader-speed-01-run-20261121.svg)](https://make.fun/bounty/csv-reader-speed-01-run-20261121?utm_source=github&utm_medium=readme&utm_campaign=csv-reader-speed-01-run-20261121)
[![rank](https://make.fun/badge/csv-reader-speed-01-run-20261121/jontumpil/makefun-csv-reader/rank.svg)](https://make.fun/bounty/csv-reader-speed-01-run-20261121/leaderboard?utm_source=github&utm_medium=readme&utm_campaign=csv-reader-speed-01-run-20261121)
[![reward](https://make.fun/badge/csv-reader-speed-01-run-20261121/jontumpil/makefun-csv-reader/reward.svg)](https://make.fun/bounty/csv-reader-speed-01-run-20261121?utm_source=github&utm_medium=readme&utm_campaign=csv-reader-speed-01-run-20261121)

**csv-reader 1.0.0's exact semantics, reimplemented as a Rust → WebAssembly
JSON-to-JSON transducer over the whole cases envelope — shipped as two
builds of the one transducer, SWAR and SIMD, with the faster-capable one
selected once at load.**

For every case in `cases.json` the entry returns exactly what the reference
`readCsv(text, header)` returns — `{columns, rows, errors}` — byte-identical
to `JSON.stringify` of the reference answers, on the practice set and on
222,408 differential fuzz cases per branch per seed (two seeds, both
branches: 889,632 branch-cases) against the kit's vendor copy, with zero
mismatches.

## The shape of it

`solve.js` embeds two small WebAssembly modules and exports the contract's
`run(input)`:

- **SWAR build** (46,232 bytes; Rust source in `rust-swar/`) — the v11
  module, byte for byte (sha256
  `5d8eef534338647190a4d6af151433a42753c691d77eab53a0c73e53e984254b`,
  reproduced from source during the v12 build). Its scanners classify a
  u64 word at a time.
- **SIMD build** (47,633 bytes; Rust source in `rust-simd/`) — the same
  transducer, differing only in its scanner section: the scanners classify
  a v128 vector at a time (i8x16 equality compares; "below 0x20" as a
  sign-biased i8x16 compare acting as an unsigned one; the first matching
  lane read out of the mask's two u64 halves — no `i8x16_bitmask`).

The two crates share everything else: the envelope scan, the WTF-8
unescape, the CSV state machine, the streaming JSON emission. Either module
does all of it — JavaScript never parses the envelope and never sees a CSV
value on the hot path — and both expose the same exports and memory
contract, so the driver cannot tell them apart.

**Selection** happens once, synchronously, as `solve.js` loads: a 43-byte
probe module holding a `v128.const` is offered to `WebAssembly.validate`;
accepted means the runtime has wasm SIMD and the SIMD build is
instantiated, rejected (or any error anywhere — probing, compiling,
instantiating) means the SWAR build is, silently. Only the chosen module is
ever instantiated. `CSV_FORCE_SIMD` / `CSV_FORCE_SWAR` in the environment
force a branch (development only; the grader sets neither), and the chosen
branch is exported as `BRANCH`. `main.ts` keeps v11's cold-start shell:
`--liftoff-only` + `--no-incremental-marking` are set before any WebAssembly
compiles, then the file is read straight into the module's memory and the
answers written straight out of it.

## Why two builds

Every timed run is a fresh process on a fresh container, so the run is
always cold and nothing tiers up; net time is gross CPU minus a bare Node
startup. On the grading image the leading entries are SIMD transducers.
On the development VM used here, this runtime executes the SIMD
classification path *slower* than the SWAR path (measured below), so SIMD
speed cannot be validated locally — but SIMD correctness can, and is,
exhaustively. The dual build takes the SIMD upside where SIMD runs at full
speed and keeps the proven SWAR module everywhere else, at a measured
packaging cost of ~1% (below).

## Measurements (development VM, throttled; treat ratios, not absolutes)

Interleaved A/B, fresh `node` processes, median of 9, CPU by
`RUSAGE_CHILDREN` over the 11 MB practice set, net of a null-driver
control (160.9 ms):

| entry | gross | net |
|---|---|---|
| v11 (SWAR, single module) | 275.3 ms | 114.5 ms |
| v12, SWAR branch forced | 278.8 ms | 118.0 ms |
| v12, auto (selects SIMD here) | 315.7 ms | 154.9 ms |
| v12, SIMD branch forced | 324.0 ms | 163.1 ms |

So the dual-module packaging itself costs +1.3% gross (v12-SWAR vs v11) —
the second base64 payload, the probe, the selection — while the SIMD
branch runs ~18% behind SWAR *on this runtime*. A control experiment with
the driver's V8 flags removed (default tiering: SWAR 275.6 ms, SIMD
329.4 ms) shows the local SIMD penalty is in this runtime's SIMD execution
path, not in Liftoff code generation. These local SIMD numbers are
informational only; the branch's correctness is what is gated.

## Verification

- Practice set: 6/6 deep-identical **and** byte-identical through the
  auto, SWAR and SIMD paths (answers sha256 `bb5b6cc5…` in all three).
- Differential fuzz vs the kit's vendor csv-reader, through both `run()`
  and `transduceAll`, three envelope serializations (plain, `\uXXXX`-heavy,
  shuffled keys + junk keys) plus the boundary battery (quotes/CR at edges,
  lone and paired surrogates, BOM raw and escaped, unterminated quotes,
  junk after closing quotes): 222,408 cases, **0 mismatches**, per branch,
  on each of two seeds.
- Determinism: two driver runs per branch byte-identical.
- Clean room: the shipped files alone in a fresh directory reproduce the
  practice answers byte-identically in auto mode; `solve.js` also loads
  via `require()`.
- Sizes: `solve.js` 135,018 B (both modules embedded); whole entry
  274,738 B — well under the 1 MiB repo cap.

## Build

`./build.sh` — cargo (wasm32-unknown-unknown target, std only) builds
`rust-swar/` and `rust-simd/`, then `gen-solve.mjs` embeds both modules
into `solve.js` from `solve.template.js`.
