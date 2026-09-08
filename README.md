# xsec

Cross-sectional momentum backtests on [Nautilus Trader](https://nautilustrader.io/)
over a basket of Bybit USDT-margined linear perpetuals. Two strategies:

- `momentum`: each month, long the top decile and short the bottom decile,
  ranked by trailing return.
- `top5-momentum-filtered`: long-only, top 5 names by a composite fast/medium/
  slow momentum score, re-ranked on a configurable cadence (daily by default;
  `--holding-period iso-week` / `month`). Each re-rank trades only the change —
  drops the names that left the top 5, adds the ones that entered, holds the
  rest untouched — and it sits out in cash whenever BTC's trend regime turns
  negative.

Each run produces two per-run HTML reports alongside the log: a QuantStats
**tearsheet** (portfolio performance) and a **per-leg diagnostics** page
(attribution, long/short book, return distribution, per-period breakdown).

## Prerequisites

- Rust toolchain (edition 2024)
- [`uv`](https://docs.astral.sh/uv/) for the Python tearsheet step
- Bybit HTTP API reachable (bar history is fetched on first run and cached
  under `data/`, which is gitignored)

## End-to-end workflow

```sh
make tearsheet              # fresh run, generated UUID-7
make tearsheet UUID=<id>    # pin / re-render a specific run id
```

`make tearsheet` runs the backtest (`make backtest`) then renders both reports
(`make report`). The same `UUID` keys everything for the run.
`cargo run --bin xsec -- <strategy>` without `--uuid` generates one and
prints `run_id=<UUID>` on stdout.

## Configuring a run

A run picks its strategy with a subcommand. `cargo run --bin xsec -- --help`
lists the strategies; `cargo run --bin xsec -- <strategy> --help` lists that
strategy's knobs: `momentum` (the `make` default) and
`top5-momentum-filtered`.

**Shared flags** (every strategy):

| Flag | Default | What it does |
| --- | --- | --- |
| `--universe <file>`     | `universe.txt` | the coin universe (see below) |
| `--starting-balance <b>`| `1_000 USDT` | simulated account starting balance (USDT only) |
| `--date-start` / `--date-end` | `2020-01-01` / `2026-09-02` | backtest window (`YYYY-MM-DD`) |
| `--uuid <id>`           | fresh UUID-7 | keys `runs/<id>/` and `logs/<id>/` |

**`momentum` flags:**

| Flag | Default | What it does |
| --- | --- | --- |
| `--lookback-months <n>` | `3`   | trailing-return formation window |
| `--percentile <p>`      | `0.1` | long/short cut as a fraction of the universe (`0.1` = deciles) |
| `--risk-pct <r>`        | `0.8` | gross exposure as a fraction of account equity, per rebalance |
| `--long-w <w>`          | `0.5` | share of the gross budget on the long side (`0.5` = dollar-neutral) |
| `--signal-tilt <t>`     | `0.0` | within-side lean toward higher-conviction names (`0` = equal weight) |
| `--holding-months <n>`  | `1`   | holding period; only `1` is supported today |

**`top5-momentum-filtered` flags:**

| Flag | Default | What it does |
| --- | --- | --- |
| `--fast-days <n>`             | `1`   | fast-momentum lookback, in daily bars |
| `--medium-days <n>`           | `3`   | medium-momentum lookback, in daily bars |
| `--slow-days <n>`             | `7`   | slow-momentum lookback, in daily bars |
| `--fast-weight <w>`           | `0.3` | weight on fast momentum in the composite score |
| `--medium-weight <w>`         | `0.0` | weight on medium momentum in the composite score |
| `--slow-weight <w>`           | `0.7` | weight on slow momentum in the composite score |
| `--top-n <n>`                 | `5`   | number of names held long at a time |
| `--regime-lookback-days <n>`  | `20`  | BTC trailing-return window for the regime filter |
| `--risk-fraction <r>`         | `0.8` | gross exposure as a fraction of account equity, per rebalance (long-only, so this is net exposure too) |
| `--allocation-tilt <t>`       | `0.0` | within-book lean toward higher-conviction names (`0` = equal weight) |
| `--holding-period <unit>`     | `day` | rebalance clock unit: `day`, `iso-week` or `month` |
| `--number-holding-periods <n>`| `1`   | `--holding-period` units between re-ranks (each re-rank trades only the top-`n` delta; survivors ride) |

The universe must include `BTC` (case-insensitive) — the regime filter reads
its trailing return from the same buffer, no separate subscription.

Invalid combinations are rejected before the engine boots (e.g. a `--percentile`
outside `(0, 0.5]`, a universe too small for the requested cut, a `--top-n`
larger than the universe, a universe missing `BTC`, `--date-start` after
`--date-end`, a non-USDT balance). The resolved values — and the exact command
line — are written to `runs/<UUID>/config.csv`.

Through `make`, pass strategy flags with `ARGS` (and pick the strategy with
`STRATEGY`):

```sh
make tearsheet ARGS="--lookback-months 6 --percentile 0.2"
make tearsheet STRATEGY=top5-momentum-filtered ARGS="--top-n 3"
```

### The universe file

`universe.txt` at the repo root is the traded universe: one base asset per line
(`BTC`, `ETH`, …), each traded as `<SYM>USDT-LINEAR.BYBIT`. Blank lines and
lines starting with `#` are ignored, as is an inline `# …` after a symbol;
symbols are upper-cased and de-duplicated. Point `--universe` at another file to
run a different basket without touching the default.

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
