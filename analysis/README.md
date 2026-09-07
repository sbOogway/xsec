# analysis/ — tearsheet, per-leg & parameter-search CLIs

Report generators and a parameter search over a backtest run's CSV artifacts.
Pure Python, driven by [`uv`](https://docs.astral.sh/uv/); the Rust backtest
and these scripts share nothing but the `runs/` directory (`optimize.py` also
drives the Rust binary directly — see below).

- `tearsheet.py` — a [QuantStats](https://github.com/ranaroussi/quantstats)
  HTML tearsheet from the portfolio return series.
- `legs.py` — per-leg diagnostics (attribution, long/short book, return
  distribution, monthly breakdown) from `legs.csv`.
- `optimize.py` — an [Optuna](https://optuna.org/) search over the momentum
  strategy's flags, with an in-sample / out-of-sample split so the search
  can't just overfit the whole backtest window.

## Setup

```sh
uv sync --project analysis   # creates analysis/.venv from analysis/pyproject.toml
```

## Usage

Run from the repo root. `--project analysis` points `uv` at this directory's
env regardless of the working directory:

```sh
# render a specific run (keys runs/<uuid>/)
uv run --project analysis analysis/tearsheet.py --uuid 0193abcd-...
uv run --project analysis analysis/legs.py --uuid 0193abcd-...

# ...or the most recently modified run
uv run --project analysis analysis/tearsheet.py --latest
uv run --project analysis analysis/legs.py --latest
```

`make tearsheet` runs both after the backtest.

`tearsheet.py` writes `runs/<uuid>/tearsheet.html` — a single self-contained
file (styles inlined, charts embedded as base64 SVG; the only external reference
is a favicon). The run id is in the `<title>` and the page heading.

`legs.py` writes `runs/<uuid>/legs.html` — also self-contained (inline CSS,
charts as base64 PNG, no external references). Four sections:

- **Per-instrument attribution** — legs, win rate, mean/median return and total
  USDT PnL (`per_leg_return × notional_usdt`) per instrument, plus a
  best/worst-contributors bar chart.
- **Long vs short book** — per-side stats and cumulative/monthly PnL by book.
- **Leg return distribution** — hit rate, avg win/loss, payoff, skew/kurtosis,
  a long/short histogram and the 10 best/worst legs.
- **Per rebalance period leg breakdown** — leg count, mean return, dispersion,
  min/max and the long−short spread each period.

A leg "wins" when `per_leg_return > 0` (a flat leg is not a win).

## Parameter search (`optimize.py`)

```sh
uv run --project analysis analysis/optimize.py                                # 50 trials, defaults
uv run --project analysis analysis/optimize.py --n-trials 200 --study-name my-study
uv run --project analysis analysis/optimize.py --skip-build                   # reuse target/release/xsec
```

Builds `target/release/xsec` once (skip with `--skip-build` if it's already
current), then splits `--date-start`/`--date-end` chronologically
(`--split-ratio`, default `0.7`) into an in-sample slice and a trailing
out-of-sample slice, aligned to month boundaries so the cut never falls inside
a rebalance month. Each of `--n-trials` trials draws a `momentum` param set
from an Optuna TPE sampler and runs a real `xsec momentum` backtest over the
**in-sample** slice only:

| param | range |
| --- | --- |
| `lookback_months` | int 1–12 |
| `percentile` | float 0.05–0.5 |
| `long_w` | float 0.0–1.0 |
| `signal_tilt` | float 0.0–3.0 |
| `risk_pct` | float 0.1–1.5 |

(`holding_months` stays fixed at the CLI default, `1` — the strategy doesn't
support other values.) The objective is CAGR computed from the trial's
`portfolio.csv` `net_return` series. A trial whose param combination makes
`xsec` exit non-zero is pruned, not fatal to the study.

Every trial's `runs/<uuid>/` is a normal backtest run — nothing is deleted —
so `tearsheet.py --uuid <trial-uuid>` / `legs.py --uuid <trial-uuid>` work on
any trial afterward. Trial uuids are `<study-name>-trial<NNNN>`.

After the study, the best in-sample trial's params are re-run once against
the **out-of-sample** slice — data the search never touched — and both CAGRs
are printed side by side; a big gap flags overfitting more directly than the
in-sample number alone. That run's uuid is `<study-name>-oos-best`.

The study is persisted to `runs/optuna/<study-name>.db` (sqlite; re-run with
the same `--study-name` to add more trials to it), and a small summary —
window dates, best params, both CAGRs, both run uuids — is written to
`runs/optuna/<study-name>.json`.

**Caveat:** the strategy needs `lookback_months` monthly bars to accumulate
before it trades at all (the warm-up request in `start_universe`,
`src/strategy/runtime.rs`). If the out-of-sample slice is shorter than the
search space's longest `lookback_months` (12), a trial that picked a long
lookback can show a flat 0% out-of-sample — it never got a chance to trade —
which is a too-short window, not evidence of overfitting. `optimize.py` warns
on stderr when this is possible; the default window (2020 → today) leaves
plenty of margin, but a short `--date-start`/`--date-end` for a quick smoke
test can trip it.

## Inputs

| File | Used for |
| --- | --- |
| `runs/<uuid>/portfolio.csv` | `tearsheet.py`: headline return series — the `net_return` column, indexed by `period_end_date` |
| `runs/<uuid>/legs.csv`      | `legs.py`: per-leg attribution, book split, return distribution and per-period breakdown |
| `runs/<uuid>/config.csv`    | not read yet; documents the run's parameters |
| `runs/<uuid>/fills.csv`     | not read yet; per-`OrderFilled` rows for future per-trade attribution |

Both CLIs exit non-zero (and say why) on an unknown run id — listing the runs
they can see — or on an empty / header-only input CSV. `legs.py` also fails if
`legs.csv` is missing required columns; `tearsheet.py` fails if `quantstats` is
not installed.

## Return definition (v1)

`legs.csv` per-leg return is a **close-to-close holding-period return**:
`(exit_price - entry_price) / entry_price`, signed by side, where `entry_price`
is the last completed bar close before the entry rebalance and `exit_price` is
that instrument's close one rebalance later. This is **price return only** —
no funding-rate carry on the perpetual leg (a future feature).

`portfolio.gross_return` is an **account-level** per-rebalance-period return:
the period's summed leg PnL — each leg's close-to-close return times its USDT
notional — divided by the period's *opening* equity. `portfolio.net_return` is
`gross_return - fee_paid_usdt / equity_start_of_period`. Because the divisor is
equity (not deployed notional), compounding the `net_return` series tracks the
`equity_end_of_period_usdt` curve rather than running ~3× ahead of it.

It still won't tie out *exactly* against `equity_end_of_period_usdt` deltas —
the leg returns are close-to-close bar math while the equity series comes from
Nautilus' simulated-margin account model (fill prices, mark timing, and funding
all differ) — but the two are now the same order of magnitude. Treat
`net_return` as the strategy signal and the equity column as the accounting
cross-check.

`period` / `period_end_date` are cadence-agnostic: a monthly strategy's period
label looks like `2026-03` (end date the month's last day), a weekly one's
looks like `2026-W12` (end date that ISO week's Sunday) — both are just
implementations of the same `RebalancePeriod` trait
(`src/period.rs`) driving `RunCapture` (`src/capture.rs`).

## Tests

```sh
uv run --project analysis pytest analysis/tests/
```
