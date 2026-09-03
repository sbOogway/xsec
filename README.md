# xsectional-rs

A cross-sectional momentum backtest: a [Nautilus Trader](https://nautilustrader.io/)
strategy that each month goes long the top decile and short the bottom decile of
a basket of Bybit USDT-margined linear perpetuals, ranked by trailing return.

Each run produces a per-run **tearsheet** — a QuantStats HTML performance report
— alongside the log.

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

`make tearsheet` runs the backtest (`make backtest`) then renders the tearsheet
(`make report`). The same `UUID` keys everything for the run. The bare
`cargo run --bin xsectional-rs` without `--uuid` generates one and prints
`run_id=<UUID>` on stdout.

## Artifacts

Everything for a run lives under a per-UUID directory:

| Path | What it is |
| --- | --- |
| `logs/<UUID>/logs.log`          | the full run log (`lnav logs/<UUID>/logs.log` to browse) |
| `runs/<UUID>/config.csv`        | the strategy configuration for the run (`key,value`) |
| `runs/<UUID>/legs.csv`          | one row per (entry month, instrument) leg, with per-leg return |
| `runs/<UUID>/portfolio.csv`     | one row per rebalance month — the aggregate return series |
| `runs/<UUID>/fills.csv`         | one row per `OrderFilled` event (fill price, quantity, fee) |
| `runs/<UUID>/tearsheet.html`    | the QuantStats tearsheet (self-contained; open in any browser) |

`runs/` is per-machine, regenerable state — gitignored, like `target/`. The
source-of-truth record of a run is its log.

See [`analysis/README.md`](analysis/README.md) for the tearsheet CLI, its inputs,
and the exact return definitions.

## Tests

```sh
cargo test                      # Rust: schema-contract smoke test + unit tests
uv run pytest analysis/tests/   # Python: tearsheet CLI
```
