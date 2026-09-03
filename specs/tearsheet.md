# Tearsheet for Cross-Sectional Momentum Backtest

## Problem Statement

The cross-sectional momentum backtest in this repo (`xsectional-rs`, a Nautilus Trader strategy that goes long the top decile and short the bottom decile of a basket of Bybit USDT-margined linear perpetuals on monthly bars) currently emits **only log lines**. There is no persisted return stream, no equity curve, no drawdown series, no performance summary.

The user runs many variants of this backtest (different lookback, percentile, holding window, date range), each tagged with a UUID-7 (the existing invocation is `UUID=$(uuidgen -7) && cargo run --bin xsectional-rs > logs/$UUID.log && lnav logs/$UUID.log`). To evaluate which variants are working, they need a per-run performance report — a tearsheet — they can open in a browser or compare side-by-side. Today the only option is to read the logs by eye.

The desired outcome: **each backtest run writes two CSV files under `runs/<UUID>.{legs,portfolio}.csv`, and a Python CLI renders a QuantStats HTML tearsheet from those files.** No notebook. No external service. Re-runnable on any past UUID.

## Solution

Two seams, language-bridged by CSV:

1. **Rust capture** in the existing `XSectionalMomentum::on_time_event` (the only place that decides which symbols go long / short each month). At the same point where positions are submitted, also write:
   - `runs/<UUID>.legs.csv` — one row per (month, instrument) the strategy touched, with side and per-leg return.
   - `runs/<UUID>.portfolio.csv` — one row per month with the portfolio return, positions count, fees, and a reference to the fills.

2. **Python tearsheet CLI** at `analysis/tearsheet.py`. Takes `--uuid <X>` or `--latest`, reads the two CSVs, builds the portfolio return series, and writes `tearsheet_<UUID>.html` via QuantStats. Runs in a `uv` virtualenv.

The UUID is the same UUID the user already passes to the backtest log; the backtest binary accepts it as an argument (default: fresh `uuidgen -7`), and both the logs and the `runs/` files end up keyed by it.

## User Stories

### Capture

1. As a researcher running a single backtest, I want the run to produce CSV files I can open in any tool, so that I am not locked into a notebook.
2. As a researcher running many variants, I want each run's data files to be keyed by the UUID I already see in `logs/`, so that I can correlate a tearsheet with the log that produced it.
3. As a researcher, I want the per-leg returns captured at the same point the strategy submits orders, so that leg and portfolio numbers are guaranteed to use the same instrument set and timing.
4. As a researcher, I want the portfolio return computed from the backtester's accounting (not from bar math) so that fees and fills are accounted for.
5. As a researcher, I want each CSV row stamped with the month, so that I can join legs and portfolio rows by month in pandas without guessing.
6. As a researcher, I want fills persisted (a row per `OrderFilled` event) so that a downstream tearsheet can attribute P&L to specific trades and apply realistic transaction costs.
7. As a researcher, I want fees captured as a per-month aggregate, so that I can subtract them from gross return when I want to.

### Persisted layout

8. As a researcher, I want `runs/<UUID>.legs.csv` and `runs/<UUID>.portfolio.csv` to be the only two artifacts of a run, so that the layout is easy to teach and to script.
9. As a researcher, I want both files to be written with a stable schema (header + column order), so that downstream tooling can rely on column names.
10. As a researcher, I want rows appended, not overwritten, so that if the binary is restarted mid-run, partial data is preserved.
11. As a researcher, I want the CSV writes to be tolerant of missing directories (create `runs/` on demand).
12. As a researcher, I want each file's first row to be the strategy's configuration (lookback months, holding months, percentile, date window, base list, starting balance), so that the tearsheet can label itself and so that file headers alone are not a maintenance hazard.

### Python tearsheet

13. As a researcher, I want `uv run analysis/tearsheet.py --uuid <X>` to produce `tearsheet_<X>.html` next to the CSVs, so that the workflow is one command from CSV to report.
14. As a researcher, I want `uv run analysis/tearsheet.py --latest` to resolve to the most recently modified `runs/*.legs.csv`, so that I do not have to copy UUIDs by hand.
15. As a researcher, I want the tearsheet to be a QuantStats full tearsheet (Sharpe, Sortino, Calmar, drawdown series, monthly returns heatmap, rolling vol), so that I get an industry-standard report.
16. As a researcher, I want the tearsheet to be saved as a single HTML file (no external assets), so that I can mail it, archive it, or open it offline.
17. As a researcher, I want the Python script to fail loudly if a UUID is unknown, listing the available UUIDs, so that I do not silently produce an empty report.

### Environment & developer experience

18. As a developer, I want `uv` to be the only Python tool needed (no global pip, no manual venv), so that the tearsheet step is reproducible from a fresh clone.
19. As a developer, I want `analysis/pyproject.toml` (or `requirements.txt` consumed by `uv`) to pin QuantStats, pandas, and numpy, so that the tearsheet's output is reproducible across machines.
20. As a developer, I want the README to document the end-to-end flow (`cargo run -- --uuid X` followed by `uv run analysis/tearsheet.py --uuid X`), so that the workflow survives a context clear.

### Out-of-scope (negatives, to keep the scope tight)

21. As a researcher, I do **not** want a Jupyter notebook; the tearsheet script must be a plain Python module.
22. As a researcher, I do **not** want the Rust binary to invoke Python; the two languages stay decoupled and share only the CSVs.
23. As a researcher, I do **not** want auto-discovery of all runs or a multi-run comparison tearsheet in this iteration; per-run only.
24. As a researcher, I do **not** want Parquet in this iteration; CSV only, even though it's larger.

## Implementation Decisions

### Rust side — data capture

- The capture lives in the strategy itself (`XSectionalMomentum`), not in Nautilus portfolio events. Reason: `on_time_event` already does the leg-selection logic; piggybacking capture there guarantees leg and portfolio rows agree on which instruments and what month.
- The capture writes two files, opened in append mode at `on_start` (creating `runs/` if missing):
  - `<RUN_DIR>/<UUID>.legs.csv`
  - `<RUN_DIR>/<UUID>.portfolio.csv`
- `UUID` is a new constructor field on `XSectionalMomentum`, populated from a CLI argument (see below). Default is a fresh `uuidgen -7` if the binary is invoked without `--uuid`.
- Each file's first non-header line is a single-row header containing the run config (run_id, generated_at, lookback_months, holding_months, percentile, date_start, date_end, bases_csv, starting_balance). One config row at the top makes the file self-describing without a sidecar JSON.

### Per-leg row schema (`<UUID>.legs.csv`)

```
run_id, month, instrument_id, side, entry_bar_open, exit_bar_close, per_leg_return, notional_usdt
```

- `month` is the calendar month of the bar that triggered the order (UTC, `YYYY-MM`).
- `side` is `long` or `short` (the side of the submitted market order).
- `entry_bar_open` is the bar's open price, decimal-stringified.
- `exit_bar_close` is the **next** month's bar close (the price used for the 1-month hold return).
- `per_leg_return` = `(exit_bar_close - entry_bar_open) / entry_bar_open`, signed by side (so short legs are negative of the bar return). Decimal, 6 dp.
- `notional_usdt` = the `DOLLAR_POSITION_SIZE` constant at the time the order was submitted (so that the portfolio aggregate can weight correctly even if it changes between runs).

### Portfolio row schema (`<UUID>.portfolio.csv`)

```
run_id, month, n_long, n_short, gross_return, fee_paid_usdt, net_return, equity_end_of_month_usdt, n_fills, fills_ref
```

- `gross_return` is the notional-weighted sum of `per_leg_return` over the legs submitted in that month, normalized by total notional deployed.
- `fee_paid_usdt` is the per-month aggregate of trading fees reported by the backtest engine (queried via the strategy's account balance deltas).
- `net_return` = `gross_return - fee_paid_usdt / equity_start_of_month_usdt`.
- `equity_end_of_month_usdt` is the cash + position value reported by the backtest account.
- `n_fills` is the number of `OrderFilled` events that month.
- `fills_ref` is the path to a sibling per-run `fills.csv` (e.g. `runs/<UUID>.fills.csv`) for traceability.

### Fills file schema (`runs/<UUID>.fills.csv`)

```
run_id, ts_event, instrument_id, side, order_side, quantity, fill_price, fee_usdt
```

- One row per `OrderFilled` event.
- Persisted so a future richer tearsheet (per-trade attribution, factor regressions) can be built without re-running the backtest.

### Return definition

- The per-leg return is **bar-to-bar over the holding period**: `(close_{M+1} - open_M) / open_M`, signed by side.
- Rationale: matches `submit_notional_market`, which uses the next bar's open as the fill proxy via `bar_at_index(&bt, 1)`. Using `close_{M+1}` for the exit is consistent with the "1-month hold" parameter and avoids inventing a different timing assumption for the report than the strategy uses for execution.
- Risk to flag in the spec: this is **price return only**, no funding-rate carry. That is intentional for v1; funding carry is a future feature.

### CLI surface for the Rust binary

- `cargo run -- --uuid <X>` (or `--uuid-file <path>`) → explicit UUID.
- `cargo run` (no flag) → UUID generated as `uuidgen -7` at startup, **also printed on stdout** so the user can capture it the same way they do for the log filename. The `logs/$UUID.log` workflow continues unchanged; the user wraps both the log redirect and (in a follow-up iteration) the `--uuid` flag in the same UUID.
- `RUST_LOG` continues to control verbosity.

### Python side — tearsheet

- One file: `analysis/tearsheet.py`.
- Stack: QuantStats (full HTML tearsheet), pandas, numpy. Pinned in `analysis/pyproject.toml`.
- Invocations:
  - `uv run analysis/tearsheet.py --uuid <X>` → reads `runs/<X>.legs.csv` and `runs/<X>.portfolio.csv`, writes `runs/<X>.tearsheet.html`.
  - `uv run analysis/tearsheet.py --latest` → picks the `*.legs.csv` with the largest mtime under `runs/`.
- Behavior:
  - Index the portfolio CSV by `month`, take the `net_return` column, build a `pandas.Series` indexed by month-end timestamps.
  - Pass to `quantstats.stats.reports.full(...)` (or the v0.6+ equivalent), passing `title=f"X-Sectional Momentum — {uuid}"`.
  - The legs file is **not** used to compute the headline return (that comes from the backtester's accounting). The legs file is the substrate for future per-leg diagnostics; v1 just verifies it exists and is non-empty.
- Failure modes: missing UUID → exit non-zero with a list of available UUIDs. Empty `portfolio.csv` → exit non-zero with a clear message. QuantStats import failure → hint at `uv sync`.

### Environment management

- `uv` (the Astral tool) is the only Python tool added to the repo. No `venv/`, no `requirements.txt` checked in; `analysis/pyproject.toml` is the source of truth.
- `uv sync` (run inside `analysis/`) sets up the environment; `uv run ...` invokes the script in it.
- The README documents this with one line, so a fresh clone needs only `uv` pre-installed.

### File layout (new)

```
analysis/
├── pyproject.toml          # quantstats, pandas, numpy, pinned
├── README.md               # one-liner usage
└── tearsheet.py            # the CLI
runs/                       # gitignored
└── <UUID>.{legs,portfolio,fills}.csv, <UUID>.tearsheet.html
```

`runs/` is added to `.gitignore`. CSVs and HTMLs are local artifacts; the source-of-truth log is `logs/<UUID>.log`.

### Testing decisions

- **What makes a good test here**: the schema is the contract. Tests assert (a) the two CSVs exist after a single backtest run, (b) headers match the spec, (c) row counts are within expected bounds, (d) `portfolio.net_return` for month M equals `sum(legs.per_leg_return * legs.notional_usdt)` over month M minus fees, (e) the tearsheet HTML is non-empty and references the UUID in its title.
- **Where to test**: a Rust integration test in `tests/capture_smoke.rs` that invokes the strategy with a small fixture (a 2-symbol universe over 6 months) and asserts the schema. A Python test in `analysis/tests/test_tearsheet.py` that constructs a tiny `portfolio.csv` fixture and asserts the script emits an HTML containing the UUID.
- **What to NOT test**: the contents of the tearsheet HTML beyond the title (QuantStats internals); the exact fee numbers (those depend on Nautilus' simulated venue config and are out of scope here).
- **Fixture strategy**: keep tests deterministic by using a tiny `BASES` slice and a date window of 6 months. Reuse the existing `cargo test` harness.

## Out of Scope

- Per-trade / per-fill attribution in the HTML tearsheet (the fills file is persisted but not yet consumed).
- Funding-rate carry on the perpetual leg.
- Multi-run comparison tearsheet.
- A `pyproject.toml` at the repo root (Python lives under `analysis/`).
- Live-trading run capture (only backtest capture in this iteration; the live branch is gated by `Environment::Live` and currently a stub).
- Replacing the existing log-based workflow with structured logging.
- A CI step that runs the tearsheet on every PR (that's a follow-up once the artifacts are stable).

## Further Notes

- The user already wraps the backtest in `UUID=$(uuidgen -7) && cargo run --bin xsectional-rs > logs/$UUID.log && lnav logs/$UUID.log`. The new binary accepts `--uuid` so the same UUID can be used to key both the log and the `runs/` files. In a follow-up, the `cargo run` invocation can be tightened to `UUID=$(uuidgen -7) && cargo run --bin xsectional-rs -- --uuid "$UUID" > logs/$UUID.log`.
- The decimal precision used in `per_leg_return` (6 dp) is chosen so that QuantStats' default plotly scales don't drop significant digits; if a leg is so small it underflows at 6 dp, that's a signal the position size / price ratio is off, not a precision bug.
- `runs/` is per-machine state; treat it like `target/` (large, regenerable, gitignored).
- QuantStats emits a self-contained HTML by default in v0.6+; if the version pinned ends up externalizing assets, the tearsheet script will inline them and we'll capture that in a follow-up ADR.