# analysis/ — tearsheet CLI

Renders a [QuantStats](https://github.com/ranaroussi/quantstats) HTML tearsheet
from a backtest run's CSV artifacts. Pure Python, driven by [`uv`](https://docs.astral.sh/uv/);
the Rust backtest and this script share nothing but the `runs/` directory.

## Setup

```sh
uv sync            # inside analysis/, creates .venv from pyproject.toml
```

## Usage

```sh
# render a specific run (keys runs/<uuid>/)
uv run analysis/tearsheet.py --uuid 0193abcd-...

# ...or the most recently modified run
uv run analysis/tearsheet.py --latest
```

Writes `runs/<uuid>/tearsheet.html` — a single self-contained file (styles
inlined, charts embedded as base64 SVG; the only external reference is a
favicon). The run id is in the `<title>` and the page heading.

## Inputs

| File | Used for |
| --- | --- |
| `runs/<uuid>/portfolio.csv` | headline return series — the `net_return` column, indexed by month-end |
| `runs/<uuid>/legs.csv`      | verified non-empty only; substrate for future per-leg diagnostics |
| `runs/<uuid>/config.csv`    | not read yet; documents the run's parameters |
| `runs/<uuid>/fills.csv`     | not read yet; per-`OrderFilled` rows for future per-trade attribution |

The CLI exits non-zero (and says why) on an unknown run id — listing the runs it
can see — on an empty `portfolio.csv`, or if `quantstats` is not installed.

## Return definition (v1)

`legs.csv` per-leg return is a **close-to-close holding-period return**:
`(exit_price - entry_price) / entry_price`, signed by side, where `entry_price`
is the last completed monthly bar close before the entry rebalance and
`exit_price` is that instrument's close one rebalance later. This is **price
return only** — no funding-rate carry on the perpetual leg (a future feature).

`portfolio.gross_return` is an **account-level** monthly return: the month's
summed leg PnL — each leg's close-to-close return times its USDT notional —
divided by the month's *opening* equity. `portfolio.net_return` is
`gross_return - fee_paid_usdt / equity_start_of_month`. Because the divisor is
equity (not deployed notional), compounding the `net_return` series tracks the
`equity_end_of_month_usdt` curve rather than running ~3× ahead of it.

It still won't tie out *exactly* against `equity_end_of_month_usdt` deltas — the
leg returns are close-to-close bar math while the equity series comes from
Nautilus' simulated-margin account model (fill prices, mark timing, and funding
all differ) — but the two are now the same order of magnitude. Treat
`net_return` as the strategy signal and the equity column as the accounting
cross-check.

## Tests

```sh
uv run pytest analysis/tests/
```
