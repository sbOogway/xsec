# xsec

A cross-sectional momentum backtest: a [Nautilus Trader](https://nautilustrader.io/)
strategy that each month goes long the top decile and short the bottom decile of
a basket of Bybit USDT-margined linear perpetuals, ranked by trailing return.

Each run produces two per-run HTML reports alongside the log: a QuantStats
**tearsheet** (portfolio performance) and a **per-leg diagnostics** page
(attribution, long/short book, return distribution, monthly breakdown).

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
strategy's knobs. Today there is one: `momentum` (the `make` default).

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

Invalid combinations are rejected before the engine boots (e.g. a `--percentile`
outside `(0, 0.5]`, a universe too small for the requested cut, `--date-start`
after `--date-end`, a non-USDT balance). The resolved values — and the exact
command line — are written to `runs/<UUID>/config.csv`.

Through `make`, pass strategy flags with `ARGS` (and pick the strategy with
`STRATEGY`):

```sh
make tearsheet ARGS="--lookback-months 6 --percentile 0.2"
```

### The universe file

`universe.txt` at the repo root is the traded universe: one base asset per line
(`BTC`, `ETH`, …), each traded as `<SYM>USDT-LINEAR.BYBIT`. Blank lines and
lines starting with `#` are ignored, as is an inline `# …` after a symbol;
symbols are upper-cased and de-duplicated. Point `--universe` at another file to
run a different basket without touching the default.

## Artifacts

Everything for a run lives under a per-UUID directory:

| Path | What it is |
| --- | --- |
| `logs/<UUID>/logs.log`          | the full run log (`lnav logs/<UUID>/logs.log` to browse) |
| `runs/<UUID>/config.csv`        | the resolved run configuration (`key,value`) — the strategy name, its knobs, the shared flags, the universe file, and the command line |
| `runs/<UUID>/legs.csv`          | one row per (entry month, instrument) leg, with per-leg return |
| `runs/<UUID>/portfolio.csv`     | one row per rebalance month — the aggregate return series |
| `runs/<UUID>/fills.csv`         | one row per `OrderFilled` event (fill price, quantity, fee) |
| `runs/<UUID>/tearsheet.html`    | the QuantStats tearsheet (self-contained; open in any browser) |
| `runs/<UUID>/legs.html`         | the per-leg diagnostics report (self-contained; open in any browser) |

`runs/` is per-machine, regenerable state — gitignored, like `target/`. The
source-of-truth record of a run is its log.

See [`analysis/README.md`](analysis/README.md) for the report CLIs, their inputs,
and the exact return definitions.

## Tests

```sh
cargo test                                         # Rust: schema-contract smoke test + unit tests
uv run --project analysis pytest analysis/tests/   # Python: tearsheet & per-leg CLIs
```
