"""Render a per-leg diagnostics HTML report from a backtest run's ``legs.csv``.

Usage:
    uv run --project analysis analysis/legs.py --uuid <RUN_ID>
    uv run --project analysis analysis/legs.py --latest

Reads ``runs/<RUN_ID>/legs.csv`` (the per-leg close-to-close returns the Rust
backtest writes) and produces ``runs/<RUN_ID>/legs.html`` — a single
self-contained file (inline CSS, charts embedded as base64 PNG, no external
references). It complements the portfolio-level QuantStats ``tearsheet.html``
built by ``analysis/tearsheet.py``; the two share nothing but the ``runs/``
directory and these mirrored CLI conventions.

Per-leg USDT PnL is ``per_leg_return * notional_usdt``; a leg "wins" when
``per_leg_return > 0`` (a flat leg is not a win). See ``analysis/README.md``.
"""

from __future__ import annotations

import argparse
import base64
import io
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
RUNS_DIR = REPO_ROOT / "runs"

REQUIRED_COLUMNS = {
    "month",
    "instrument_id",
    "side",
    "entry_price",
    "exit_price",
    "per_leg_return",
    "notional_usdt",
}


def _fail(message: str) -> "NoReturn":  # type: ignore[name-defined]
    print(f"error: {message}", file=sys.stderr)
    raise SystemExit(1)


# -- CLI plumbing (mirrors analysis/tearsheet.py) ----------------------------


def available_uuids() -> list[str]:
    """Run ids that have a legs CSV under ``runs/<uuid>/``, newest first."""
    legs = sorted(RUNS_DIR.glob("*/legs.csv"), key=lambda p: p.stat().st_mtime, reverse=True)
    return [p.parent.name for p in legs]


def resolve_uuid(args: argparse.Namespace) -> str:
    if args.latest:
        uuids = available_uuids()
        if not uuids:
            _fail(f"no runs found under {RUNS_DIR} (looked for */legs.csv)")
        return uuids[0]
    return args.uuid


def load_legs(uuid: str):
    """Load and lightly enrich ``runs/<uuid>/legs.csv``.

    Adds ``pnl_usdt`` (per-leg USDT PnL), ``is_win`` (return strictly positive)
    and ``month_dt`` (month-end timestamp) columns.
    """
    import pandas as pd

    legs_path = RUNS_DIR / uuid / "legs.csv"

    if not legs_path.exists():
        known = available_uuids()
        listing = "\n  ".join(known) if known else "(none)"
        _fail(f"unknown run '{uuid}': no {uuid}/legs.csv. Available runs:\n  {listing}")

    if legs_path.stat().st_size == 0:
        _fail(f"legs CSV is empty for run {uuid} ({uuid}/legs.csv)")

    legs = pd.read_csv(legs_path)
    if legs.empty:
        _fail(f"legs CSV has a header but no rows for run {uuid}")

    missing = REQUIRED_COLUMNS - set(legs.columns)
    if missing:
        _fail(f"legs CSV for run {uuid} is missing columns: {', '.join(sorted(missing))}")

    legs["per_leg_return"] = legs["per_leg_return"].astype(float)
    legs["notional_usdt"] = legs["notional_usdt"].astype(float)
    legs["pnl_usdt"] = legs["per_leg_return"] * legs["notional_usdt"]
    legs["is_win"] = legs["per_leg_return"] > 0
    legs["month_dt"] = pd.to_datetime(legs["month"], format="%Y-%m") + pd.offsets.MonthEnd(0)
    return legs.sort_values(["month_dt", "instrument_id"]).reset_index(drop=True)


# -- rendering helpers ------------------------------------------------------


def _fig_to_img(fig) -> str:
    """Serialise a matplotlib figure to an inline ``<img>`` (base64 PNG)."""
    import matplotlib.pyplot as plt

    buf = io.BytesIO()
    fig.savefig(buf, format="png", dpi=110, bbox_inches="tight", facecolor="white")
    plt.close(fig)
    encoded = base64.b64encode(buf.getvalue()).decode("ascii")
    return f'<img alt="chart" src="data:image/png;base64,{encoded}">'


def _pct(x: float) -> str:
    return f"{x * 100:.2f}%"


def _usd(x: float) -> str:
    return f"{x:,.2f}"


def _table(df, *, index: bool = False) -> str:
    return df.to_html(index=index, border=0, classes="grid", float_format=lambda v: f"{v:,.4f}")


# -- report sections ------------------------------------------------------


def section_instruments(legs) -> str:
    import matplotlib.pyplot as plt

    grouped = legs.groupby("instrument_id")
    table = grouped.agg(
        n_legs=("per_leg_return", "size"),
        n_long=("side", lambda s: (s == "long").sum()),
        n_short=("side", lambda s: (s == "short").sum()),
        win_rate=("is_win", "mean"),
        mean_return=("per_leg_return", "mean"),
        median_return=("per_leg_return", "median"),
        total_pnl_usdt=("pnl_usdt", "sum"),
    )
    table = table.sort_values("total_pnl_usdt", ascending=False)

    display = table.copy()
    display["win_rate"] = display["win_rate"].map(_pct)
    display["mean_return"] = display["mean_return"].map(_pct)
    display["median_return"] = display["median_return"].map(_pct)
    display["total_pnl_usdt"] = display["total_pnl_usdt"].map(_usd)
    display = display.reset_index().rename(
        columns={
            "instrument_id": "instrument",
            "n_legs": "legs",
            "n_long": "long",
            "n_short": "short",
            "win_rate": "win rate",
            "mean_return": "mean",
            "median_return": "median",
            "total_pnl_usdt": "PnL (USDT)",
        }
    )

    # Best/worst contributors bar chart: show every instrument when there are
    # few, otherwise just the worst 15 and best 15.
    ordered = table["total_pnl_usdt"].sort_values()
    if len(ordered) > 30:
        picked = ordered.iloc[list(range(15)) + list(range(-15, 0))]
        chart_title = "Total PnL by instrument — worst 15 and best 15"
    else:
        picked = ordered
        chart_title = "Total PnL by instrument (USDT)"

    fig, ax = plt.subplots(figsize=(9, max(3, 0.32 * len(picked))))
    colors = ["#c0392b" if v < 0 else "#27ae60" for v in picked.values]
    ax.barh(picked.index.astype(str), picked.values, color=colors)
    ax.axvline(0, color="#333", linewidth=0.8)
    ax.set_xlabel("USDT")
    ax.set_title(chart_title)
    ax.grid(axis="x", alpha=0.3)

    return (
        "<h2>Per-instrument attribution</h2>"
        "<p>One row per instrument across the whole run, sorted by total USDT PnL "
        "(<code>per_leg_return &times; notional_usdt</code>, summed).</p>"
        f"{_fig_to_img(fig)}"
        f"{_table(display)}"
    )


def section_book(legs) -> str:
    import matplotlib.pyplot as plt

    by_side = legs.groupby("side").agg(
        n_legs=("per_leg_return", "size"),
        win_rate=("is_win", "mean"),
        mean_return=("per_leg_return", "mean"),
        median_return=("per_leg_return", "median"),
        std_return=("per_leg_return", "std"),
        total_pnl_usdt=("pnl_usdt", "sum"),
    )
    display = by_side.copy()
    for col in ("win_rate", "mean_return", "median_return", "std_return"):
        display[col] = display[col].map(_pct)
    display["total_pnl_usdt"] = display["total_pnl_usdt"].map(_usd)
    display = display.reset_index().rename(
        columns={
            "side": "book",
            "n_legs": "legs",
            "win_rate": "win rate",
            "mean_return": "mean",
            "median_return": "median",
            "std_return": "std",
            "total_pnl_usdt": "PnL (USDT)",
        }
    )

    monthly_pnl = legs.pivot_table(
        index="month_dt", columns="side", values="pnl_usdt", aggfunc="sum"
    ).fillna(0.0)
    monthly_ret = legs.pivot_table(
        index="month_dt", columns="side", values="per_leg_return", aggfunc="mean"
    )

    fig, (top, bot) = plt.subplots(2, 1, figsize=(9, 7), sharex=True)
    for side in monthly_pnl.columns:
        top.plot(monthly_pnl.index, monthly_pnl[side].cumsum(), label=side, marker=".")
    top.set_title("Cumulative USDT PnL by book")
    top.set_ylabel("USDT")
    top.legend()
    top.grid(alpha=0.3)

    for side in monthly_ret.columns:
        bot.plot(monthly_ret.index, monthly_ret[side] * 100, label=side, marker=".")
    bot.axhline(0, color="#333", linewidth=0.8)
    bot.set_title("Monthly mean leg return by book")
    bot.set_ylabel("%")
    bot.legend()
    bot.grid(alpha=0.3)
    fig.autofmt_xdate()

    return (
        "<h2>Long vs short book</h2>"
        f"{_table(display)}"
        f"{_fig_to_img(fig)}"
    )


def section_distribution(legs) -> str:
    import matplotlib.pyplot as plt

    r = legs["per_leg_return"]
    wins = r[r > 0]
    losses = r[r < 0]
    avg_win = wins.mean() if not wins.empty else 0.0
    avg_loss = losses.mean() if not losses.empty else 0.0
    payoff = abs(avg_win / avg_loss) if avg_loss != 0 else float("nan")

    stats = [
        ("legs", f"{len(r)}"),
        ("hit rate", _pct(legs["is_win"].mean())),
        ("mean", _pct(r.mean())),
        ("median", _pct(r.median())),
        ("std", _pct(r.std())),
        ("skew", f"{r.skew():.3f}"),
        ("excess kurtosis", f"{r.kurt():.3f}"),
        ("avg win", _pct(avg_win)),
        ("avg loss", _pct(avg_loss)),
        ("payoff (avg win / |avg loss|)", f"{payoff:.3f}"),
        ("best leg", _pct(r.max())),
        ("worst leg", _pct(r.min())),
    ]
    stats_html = "".join(f"<tr><th>{k}</th><td>{v}</td></tr>" for k, v in stats)

    fig, ax = plt.subplots(figsize=(9, 4))
    long_r = legs.loc[legs["side"] == "long", "per_leg_return"] * 100
    short_r = legs.loc[legs["side"] == "short", "per_leg_return"] * 100
    lo, hi = r.min() * 100, r.max() * 100
    bins = 30 if len(r) > 30 else max(5, len(r))
    rng = (lo, hi) if hi > lo else None
    ax.hist([long_r, short_r], bins=bins, range=rng, stacked=True,
            label=["long", "short"], color=["#2e86de", "#e67e22"])
    ax.axvline(0, color="#333", linewidth=0.8)
    ax.set_xlabel("per-leg return (%)")
    ax.set_ylabel("legs")
    ax.set_title("Leg return distribution")
    ax.legend()
    ax.grid(axis="y", alpha=0.3)

    extremes_cols = ["month", "instrument_id", "side", "entry_price", "exit_price", "per_leg_return"]
    k = min(10, len(legs))
    best = legs.nlargest(k, "per_leg_return")[extremes_cols].copy()
    worst = legs.nsmallest(k, "per_leg_return")[extremes_cols].copy()
    for frame in (best, worst):
        frame["per_leg_return"] = frame["per_leg_return"].map(_pct)
        frame.rename(columns={"instrument_id": "instrument", "per_leg_return": "return"}, inplace=True)

    return (
        "<h2>Leg return distribution</h2>"
        f'<table class="grid kv">{stats_html}</table>'
        f"{_fig_to_img(fig)}"
        f"<h3>Best {k} legs</h3>{_table(best)}"
        f"<h3>Worst {k} legs</h3>{_table(worst)}"
    )


def section_monthly(legs) -> str:
    import matplotlib.pyplot as plt

    def long_short_spread(frame):
        by_side = frame.groupby("side")["per_leg_return"].mean()
        return by_side.get("long", float("nan")) - by_side.get("short", float("nan"))

    monthly = legs.groupby("month").agg(
        n_legs=("per_leg_return", "size"),
        mean_return=("per_leg_return", "mean"),
        dispersion=("per_leg_return", "std"),
        min_return=("per_leg_return", "min"),
        max_return=("per_leg_return", "max"),
    )
    monthly["long_short_spread"] = legs.groupby("month").apply(long_short_spread, include_groups=False)
    monthly = monthly.reset_index()

    display = monthly.copy()
    for col in ("mean_return", "dispersion", "min_return", "max_return", "long_short_spread"):
        display[col] = display[col].map(lambda v: _pct(v) if v == v else "—")
    display = display.rename(
        columns={
            "n_legs": "legs",
            "mean_return": "mean",
            "min_return": "min",
            "max_return": "max",
            "long_short_spread": "long−short",
        }
    )

    fig, ax = plt.subplots(figsize=(9, 4))
    colors = ["#27ae60" if v >= 0 else "#c0392b" for v in monthly["mean_return"]]
    ax.bar(monthly["month"].astype(str), monthly["mean_return"] * 100, color=colors)
    ax.axhline(0, color="#333", linewidth=0.8)
    ax.set_ylabel("%")
    ax.set_title("Monthly mean leg return")
    ax.grid(axis="y", alpha=0.3)
    step = max(1, len(monthly) // 24)
    ax.set_xticks(range(0, len(monthly), step))
    ax.set_xticklabels(monthly["month"].astype(str).iloc[::step], rotation=90)

    return (
        "<h2>Per-month leg breakdown</h2>"
        f"{_fig_to_img(fig)}"
        f"{_table(display)}"
    )


# -- top level ------------------------------------------------------------

_CSS = """
body { font: 14px/1.5 -apple-system, Segoe UI, Roboto, Helvetica, Arial, sans-serif;
       margin: 0 auto; max-width: 1000px; padding: 2rem 1.5rem; color: #1a1a1a; }
h1 { font-size: 1.5rem; margin-bottom: 0.2rem; }
h2 { font-size: 1.2rem; margin-top: 2.5rem; border-bottom: 2px solid #eee; padding-bottom: 0.3rem; }
h3 { font-size: 1rem; margin-top: 1.5rem; }
.sub { color: #666; margin-top: 0; }
img { display: block; margin: 1rem 0; max-width: 100%; }
table.grid { border-collapse: collapse; margin: 1rem 0; font-size: 13px; }
table.grid th, table.grid td { border: 1px solid #ddd; padding: 4px 10px; text-align: right; }
table.grid th { background: #f6f8fa; }
table.grid td:first-child, table.grid th:first-child { text-align: left; }
table.kv th { text-align: left; }
code { background: #f6f8fa; padding: 1px 4px; border-radius: 3px; }
"""


def render(uuid: str, legs) -> Path:
    output = RUNS_DIR / uuid / "legs.html"
    output.parent.mkdir(parents=True, exist_ok=True)

    title = f"X-Sectional Momentum — per-leg diagnostics — {uuid}"
    months = legs["month"].nunique()
    body = "".join(
        (
            f"<h1>Per-leg diagnostics</h1>",
            f'<p class="sub">run <code>{uuid}</code> — {len(legs)} legs over {months} months '
            f'({legs["instrument_id"].nunique()} instruments)</p>',
            section_instruments(legs),
            section_book(legs),
            section_distribution(legs),
            section_monthly(legs),
        )
    )
    html = (
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">"
        f"<title>{title}</title><style>{_CSS}</style></head><body>{body}</body></html>"
    )
    output.write_text(html)

    if output.stat().st_size == 0:
        _fail(f"produced no output at {output}")
    return output


def main(argv: list[str] | None = None) -> None:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--uuid", help="run id to render (keys runs/<uuid>/)")
    group.add_argument("--latest", action="store_true", help="render the most recently modified run")
    args = parser.parse_args(argv)

    import matplotlib

    matplotlib.use("Agg")

    uuid = resolve_uuid(args)
    legs = load_legs(uuid)
    output = render(uuid, legs)
    print(f"wrote {output.relative_to(REPO_ROOT)}  ({len(legs)} legs)")


if __name__ == "__main__":
    main()
