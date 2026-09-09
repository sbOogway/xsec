# xsec

Cross-sectional momentum backtests on [Nautilus Trader](https://nautilustrader.io/)
over a basket of USDT-margined linear perpetuals (Bybit by default; `--exchange`
picks the venue).

One strategy, `momentum`: rank the universe by a composite fast/medium/slow
momentum score, hold the top `--top-n` names long and the bottom `--short-n`
short (`--short-n 0` = long-only), re-ranked on a configurable cadence (daily by
default; `--holding-period iso-week` / `month`). Each re-rank trades only the
change — drops the names that left their slice, adds the ones that entered
(flipping a name's side if it crossed from the top slice to the bottom), holds
the rest untouched. With `--regime-filter` on it also flattens the whole book
to cash whenever BTC's trend regime turns negative.

Each run produces two per-run HTML reports alongside the log: a QuantStats
**tearsheet** (portfolio performance) and a **per-leg diagnostics** page
(attribution, long/short book, return distribution, per-period breakdown).

## Prerequisites

- Rust toolchain (edition 2024)
- [`uv`](https://docs.astral.sh/uv/) for the Python tearsheet step
- The exchange's HTTP API reachable for `make fetch` (see below). Backtests
  themselves run offline against the `data/<exchange>/` cache, which is gitignored.

## Fetch the data first

A backtest reads bar history and the instrument list from the
`data/<exchange>/` cache only — it never reaches the network. Populate it with
`xsec fetch`:

```sh
make fetch                                # bybit, universe.txt
make fetch UNIVERSE=coins/my_universe.txt
make fetch EXCHANGE=bybit                 # only bybit today
```

`xsec fetch` downloads the venue's linear-instruments list and every universe
symbol's full daily-bar history into `data/<exchange>/`, and writes
`data/<exchange>/manifest.json` (what resolved to a perp on the venue, and each
symbol's bar coverage). Re-run it to refresh; `FETCH_ARGS="--refresh"` forces a
re-download of caches that are still fresh. Coins with no USDT perp on the venue
are logged and skipped. A backtest against a missing cache stops with a pointer
to run this first.

## End-to-end workflow

```sh
make fetch                  # once — populate data/<exchange>/ from the venue
make tearsheet              # fresh run, generated UUID-7
make tearsheet UUID=<id>    # pin / re-render a specific run id
```

`make tearsheet` runs the backtest (`make backtest`) then renders both reports
(`make report`). The same `UUID` keys everything for the run.
`cargo run --bin xsec -- <strategy>` without `--uuid` generates one and
prints `run_id=<UUID>` on stdout.

## Configuring a run

A run picks its strategy with a subcommand. `cargo run --bin xsec -- --help`
lists the subcommands — `fetch` (see above) and the strategies (one strategy,
`momentum`); `cargo run --bin xsec -- momentum --help` lists its knobs.

**Shared flags:**

| Flag | Default | What it does |
| --- | --- | --- |
| `--universe <file>`     | `universe.txt` | the coin universe (see below) |
| `--exchange <venue>`    | `bybit` | which venue's `data/<venue>/` cache to fetch / read (only `bybit` today) |
| `--starting-balance <b>`| `1_000 USDT` | simulated account starting balance (USDT only) |
| `--date-start` / `--date-end` | `2020-01-01` / `2026-09-02` | backtest window (`YYYY-MM-DD`) |
| `--uuid <id>`           | fresh UUID-7 | keys `runs/<id>/` and `logs/<id>/` |

**`momentum` flags:**

| Flag | Default | What it does |
| --- | --- | --- |
| `--fast-days <n>`             | `1`   | fast-momentum lookback, in daily bars |
| `--medium-days <n>`           | `3`   | medium-momentum lookback, in daily bars |
| `--slow-days <n>`             | `7`   | slow-momentum lookback, in daily bars |
| `--fast-weight <w>`           | `0.3` | weight on fast momentum in the composite score |
| `--medium-weight <w>`         | `0.0` | weight on medium momentum in the composite score |
| `--slow-weight <w>`           | `0.7` | weight on slow momentum in the composite score |
| `--top-n <n>`                 | `5`   | number of names held long at a time (top of the score) |
| `--short-n <n>`               | `5`   | number of names held short (bottom of the score); `0` = long-only |
| `--long-w <w>`                | `0.5` | share of the gross budget on the long side (`0.5` = dollar-neutral); ignored when `--short-n 0` |
| `--regime-lookback-days <n>`  | `20`  | BTC trailing-return window for the regime filter |
| `--regime-filter <bool>`      | `false` | flatten the whole book to cash on a negative BTC trend; needs `BTC` in the universe when on |
| `--risk-fraction <r>`         | `0.8` | gross exposure as a fraction of account equity, per rebalance |
| `--allocation-tilt <t>`       | `0.0` | within-side lean toward higher-conviction names (`0` = equal weight) |
| `--holding-period <unit>`     | `day` | rebalance clock unit: `day`, `iso-week` or `month` |
| `--number-holding-periods <n>`| `1`   | `--holding-period` units between re-ranks (each re-rank trades only the top-/bottom-`n` delta; survivors ride) |

Invalid combinations are rejected before the engine boots (e.g. `--top-n` +
`--short-n` larger than the universe, `--regime-filter true` with no `BTC` in
the universe, all three score weights `0`, `--long-w` outside `[0, 1]`,
`--date-start` after `--date-end`, a non-USDT balance). The resolved values —
and the exact command line — are written to `runs/<UUID>/config.csv`.

Through `make`, pass strategy flags with `ARGS`:

```sh
make tearsheet ARGS="--slow-days 14 --top-n 3 --short-n 3"
make tearsheet ARGS="--short-n 0 --regime-filter true"
```

### The universe file

`universe.txt` at the repo root is the traded universe: one base asset per line
(`BTC`, `ETH`, …), each traded as `<SYM>USDT-LINEAR.<VENUE>` (`--exchange`).
Blank lines and lines starting with `#` are ignored, as is an inline `# …` after
a symbol; symbols are upper-cased and de-duplicated. Point `--universe` at
another file to run a different basket without touching the default.

## Parameter search

```sh
make optimize                                 # Optuna search, 50 trials, defaults
make optimize OPT_ARGS="--n-trials 200"
```

`analysis/optimize.py` runs an Optuna search over `momentum`'s flags, splitting
the backtest window chronologically into in-sample (drives the search) and
out-of-sample (validates the best trial once, after the fact) so the search
can't just overfit the whole window. See
[`analysis/README.md`](analysis/README.md#parameter-search-optimizepy).

## Artifacts

Everything for a run lives under a per-UUID directory:

| Path | What it is |
| --- | --- |
| `logs/<UUID>/logs.log`          | the full run log (`lnav logs/<UUID>/logs.log` to browse) |
| `runs/<UUID>/config.csv`        | the resolved run configuration (`key,value`) — the strategy name, its knobs, the shared flags, the universe file, and the command line |
| `runs/<UUID>/legs.csv`          | one row per (entry period, instrument) leg, with per-leg return |
| `runs/<UUID>/portfolio.csv`     | one row per rebalance period — the aggregate return series |
| `runs/<UUID>/fills.csv`         | one row per `OrderFilled` event (fill price, quantity, fee) |
| `runs/<UUID>/tearsheet.html`    | the QuantStats tearsheet (self-contained; open in any browser) |
| `runs/<UUID>/legs.html`         | the per-leg diagnostics report (self-contained; open in any browser) |
| `runs/optuna/<study-name>.db`   | an Optuna study (sqlite); every trial is also a normal `runs/<trial-uuid>/` |
| `runs/optuna/<study-name>.json` | best-trial params + in-sample/out-of-sample CAGR from `make optimize` |

`runs/` is per-machine, regenerable state — gitignored, like `target/`. The
source-of-truth record of a run is its log.

See [`analysis/README.md`](analysis/README.md) for the report CLIs, their inputs,
and the exact return definitions.

## Tests

```sh
cargo test                                         # Rust: schema-contract smoke test + unit tests
uv run --project analysis pytest analysis/tests/   # Python: tearsheet & per-leg CLIs
```
