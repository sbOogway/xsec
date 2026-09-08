"""Optuna parameter search for the momentum strategy, with an in-sample /
out-of-sample split so a study can't just overfit the whole backtest window.

Usage:
    uv run --project analysis analysis/optimize.py
    uv run --project analysis analysis/optimize.py --n-trials 100 --study-name my-study

Each trial shells out to a release build of the `xsec` binary (same one
`make backtest` uses), the same way for every trial:

    target/release/xsec --uuid <trial-uuid> --universe <universe> \\
        --starting-balance <balance> --date-start <IS-start> --date-end <IS-end> \\
        momentum --fast-days .. --slow-days .. --top-n .. --short-n .. \\
        --long-w .. --allocation-tilt .. --risk-fraction ..

so every trial's artifacts land under `runs/<trial-uuid>/` exactly like a
normal `make backtest` run, and `analysis/tearsheet.py` /
`analysis/legs.py --uuid <trial-uuid>` work on them unmodified. Nothing is
deleted after a trial — a study's `runs/` directories are its full audit
trail.

The `--date-start`/`--date-end` window is split chronologically (70/30 by
default, see `--split-ratio`), the cut aligned to a month boundary for a
stable, reproducible split: the first slice is in-sample and drives the Optuna
objective (CAGR), the second is out-of-sample and is only ever touched once,
after the study, to validate the best trial's params on data the search never
saw. The objective annualises by the run's actual calendar span, so it is
correct whatever `--holding-period` the search settles on. See
`analysis/README.md`.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from datetime import datetime
from pathlib import Path
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    import optuna

REPO_ROOT = Path(__file__).resolve().parent.parent
RUNS_DIR = REPO_ROOT / "runs"
STUDIES_DIR = RUNS_DIR / "optuna"
BINARY = REPO_ROOT / "target" / "release" / "xsec"

# name -> (kind, low, high). Mirrors the flags in src/strategy/momentum/config.rs.
# Only the continuous signal / sizing knobs are searched; the composite-score
# weights, the regime filter and the rebalance cadence stay at their CLI
# defaults (fast/slow weights 0.3 / 0.7, --regime-filter off, --holding-period
# day). A trial whose --top-n + --short-n exceeds the universe is pruned, not
# fatal.
SEARCH_SPACE: dict[str, tuple[str, float, float]] = {
    "fast_days": ("int", 1, 5),
    "slow_days": ("int", 5, 30),
    "top_n": ("int", 2, 12),
    "short_n": ("int", 0, 12),
    "long_w": ("float", 0.0, 1.0),
    "allocation_tilt": ("float", 0.0, 3.0),
    "risk_fraction": ("float", 0.1, 1.5),
}

# The strategy's warm-up: it needs `max(fast_days, medium_days, slow_days,
# regime_lookback_days) + 1` daily bars before it trades (see start_universe in
# src/strategy/common.rs). With the search space above the binding term is
# slow_days, so the longest warm-up any trial can ask for is:
MAX_WARMUP_DAYS = int(SEARCH_SPACE["slow_days"][2]) + 1


def _fail(message: str) -> "NoReturn":  # type: ignore[name-defined]
    print(f"error: {message}", file=sys.stderr)
    raise SystemExit(1)


class BacktestFailed(RuntimeError):
    """A trial's backtest subprocess exited non-zero, or its output couldn't be read."""


# -- date splitting ---------------------------------------------------------


def split_is_oos(date_start: str, date_end: str, ratio: float = 0.7) -> tuple[str, str, str, str]:
    """Split ``[date_start, date_end]`` into a leading in-sample slice and a
    trailing out-of-sample slice, the cut aligned to a month boundary.

    Returns ``(is_start, is_end, oos_start, oos_end)`` as ``YYYY-MM-DD``
    strings. ``is_start`` is ``date_start`` and ``oos_end`` is ``date_end``
    verbatim; the cut in between lands on a month boundary so the split is
    stable and reproducible. Always leaves at least one month on each side.
    """
    import pandas as pd

    if not 0.0 < ratio < 1.0:
        raise ValueError(f"split ratio must be in (0, 1), got {ratio}")

    start = pd.Timestamp(date_start)
    end = pd.Timestamp(date_end)
    if start >= end:
        raise ValueError(f"date_start ({date_start}) must be before date_end ({date_end})")

    months = pd.period_range(start, end, freq="M")
    if len(months) < 2:
        raise ValueError(
            f"range {date_start}..{date_end} spans only {len(months)} month(s); "
            "need at least 2 to split into in-sample / out-of-sample"
        )
    split_idx = min(max(round(len(months) * ratio), 1), len(months) - 1)

    is_end = months[split_idx - 1].end_time.normalize().strftime("%Y-%m-%d")
    oos_start = months[split_idx].start_time.strftime("%Y-%m-%d")
    return date_start, is_end, oos_start, date_end


def day_span(date_start: str, date_end: str) -> int:
    """Number of calendar days between `date_start` and `date_end`, inclusive."""
    import pandas as pd

    return (pd.Timestamp(date_end) - pd.Timestamp(date_start)).days + 1


# -- objective plumbing ------------------------------------------------------


def suggest_params(trial: "optuna.Trial") -> dict[str, int | float]:
    """One draw from `SEARCH_SPACE`, keyed by the momentum strategy's flag names."""
    out: dict[str, int | float] = {}
    for name, (kind, low, high) in SEARCH_SPACE.items():
        if kind == "int":
            out[name] = trial.suggest_int(name, int(low), int(high))
        else:
            out[name] = trial.suggest_float(name, low, high)
    return out


def params_to_argv(params: dict[str, int | float]) -> list[str]:
    """`{"fast_days": 3, "long_w": 0.5, ...}` -> `["--fast-days", "3", ...]`."""
    argv: list[str] = []
    for name, value in params.items():
        flag = "--" + name.replace("_", "-")
        text = str(value) if isinstance(value, int) else f"{value:.6f}"
        argv += [flag, text]
    return argv


def run_backtest(
    binary: Path,
    uuid: str,
    *,
    universe: str,
    starting_balance: str,
    date_start: str,
    date_end: str,
    strategy_argv: list[str],
) -> None:
    """Run one `xsec momentum` backtest, writing `runs/<uuid>/*.csv`.

    Raises `BacktestFailed` (never lets a bad param combination crash the
    whole study) if the process exits non-zero.
    """
    cmd = [
        str(binary),
        "--uuid",
        uuid,
        "--universe",
        universe,
        "--starting-balance",
        starting_balance,
        "--date-start",
        date_start,
        "--date-end",
        date_end,
        "momentum",
        *strategy_argv,
    ]
    result = subprocess.run(cmd, cwd=REPO_ROOT, capture_output=True, text=True)
    if result.returncode != 0:
        detail = (result.stderr or result.stdout).strip()
        raise BacktestFailed(detail or f"xsec exited {result.returncode}")


def load_portfolio(uuid: str):
    """The `runs/<uuid>/portfolio.csv` rows, sorted chronologically by period."""
    import pandas as pd

    path = RUNS_DIR / uuid / "portfolio.csv"
    if not path.exists() or path.stat().st_size == 0:
        raise BacktestFailed(f"no portfolio.csv for run {uuid}")

    frame = pd.read_csv(path)
    needed = {"net_return", "period", "period_end_date"}
    if frame.empty or not needed.issubset(frame.columns):
        raise BacktestFailed(
            f"portfolio.csv for run {uuid} is empty or missing {sorted(needed)}"
        )
    return frame.sort_values("period").reset_index(drop=True)


def cagr(frame) -> float:
    """Compound annual growth rate from a `portfolio.csv` frame.

    Compounds the per-period `net_return` series, then annualises by the run's
    actual calendar span — the number of periods times their median spacing —
    so the figure is correct whatever the rebalance cadence (daily, weekly,
    monthly). A total wipeout (or worse) floors at -1.0.
    """
    import pandas as pd

    if frame.empty:
        raise ValueError("cagr: empty return series")

    returns = frame["net_return"].astype(float)
    growth = float((1.0 + returns).prod())
    if growth <= 0.0:
        return -1.0

    dates = pd.to_datetime(frame["period_end_date"]).sort_values()
    gaps = dates.diff().dropna()
    median_gap_days = int(gaps.median().days) if not gaps.empty else 30
    median_gap_days = max(median_gap_days, 1)
    years = len(returns) * median_gap_days / 365.25
    return growth ** (1.0 / years) - 1.0


def make_objective(
    *,
    binary: Path,
    universe: str,
    starting_balance: str,
    is_start: str,
    is_end: str,
    study_name: str,
):
    """Build the per-trial objective: run the IS backtest, return its CAGR.

    A failed or unreadable trial is pruned (`optuna.TrialPruned`) rather than
    crashing the study; the failure reason is stashed in `trial.user_attrs`.
    """
    import optuna

    def objective(trial: optuna.Trial) -> float:
        params = suggest_params(trial)
        uuid = f"{study_name}-trial{trial.number:04d}"
        try:
            run_backtest(
                binary,
                uuid,
                universe=universe,
                starting_balance=starting_balance,
                date_start=is_start,
                date_end=is_end,
                strategy_argv=params_to_argv(params),
            )
            frame = load_portfolio(uuid)
        except BacktestFailed as exc:
            trial.set_user_attr("error", str(exc))
            raise optuna.TrialPruned(str(exc)) from exc

        trial.set_user_attr("run_uuid", uuid)
        trial.set_user_attr("n_periods", len(frame))
        return cagr(frame)

    return objective


def validate_oos(
    binary: Path,
    best_trial: "optuna.trial.FrozenTrial",
    *,
    universe: str,
    starting_balance: str,
    oos_start: str,
    oos_end: str,
    study_name: str,
) -> tuple[float, str]:
    """Re-run the best trial's params on the OOS window. Returns `(cagr, run_uuid)`."""
    uuid = f"{study_name}-oos-best"
    run_backtest(
        binary,
        uuid,
        universe=universe,
        starting_balance=starting_balance,
        date_start=oos_start,
        date_end=oos_end,
        strategy_argv=params_to_argv(best_trial.params),
    )
    return cagr(load_portfolio(uuid)), uuid


def write_summary(
    *,
    study_name: str,
    is_start: str,
    is_end: str,
    oos_start: str,
    oos_end: str,
    best_trial: "optuna.trial.FrozenTrial",
    oos_cagr: float,
    oos_uuid: str,
) -> Path:
    STUDIES_DIR.mkdir(parents=True, exist_ok=True)
    out = STUDIES_DIR / f"{study_name}.json"
    payload = {
        "study_name": study_name,
        "in_sample": {"start": is_start, "end": is_end},
        "out_of_sample": {"start": oos_start, "end": oos_end},
        "best_trial": {
            "number": best_trial.number,
            "run_uuid": best_trial.user_attrs.get("run_uuid"),
            "params": best_trial.params,
            "is_cagr": best_trial.value,
        },
        "oos_cagr": oos_cagr,
        "oos_run_uuid": oos_uuid,
    }
    out.write_text(json.dumps(payload, indent=2))
    return out


def ensure_release_binary(*, skip_build: bool) -> Path:
    if skip_build:
        if not BINARY.exists():
            _fail(f"--skip-build passed but {BINARY} does not exist; run `cargo build --release --bin xsec` first")
        return BINARY

    print("building release binary: cargo build --release --bin xsec")
    result = subprocess.run(["cargo", "build", "--release", "--bin", "xsec"], cwd=REPO_ROOT)
    if result.returncode != 0 or not BINARY.exists():
        _fail("cargo build --release --bin xsec failed")
    return BINARY


# -- top level ----------------------------------------------------------


def main(argv: list[str] | None = None) -> None:
    import optuna

    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--universe", default="universe.txt", help="coin universe file (default: universe.txt)")
    parser.add_argument("--starting-balance", default="1_000 USDT")
    parser.add_argument("--date-start", default="2020-01-01", help="overall window start, YYYY-MM-DD")
    parser.add_argument("--date-end", default="2026-09-02", help="overall window end, YYYY-MM-DD")
    parser.add_argument(
        "--split-ratio",
        type=float,
        default=0.7,
        help="fraction of the window that is in-sample (default: 0.7)",
    )
    parser.add_argument("--n-trials", type=int, default=50)
    parser.add_argument("--jobs", type=int, default=1, help="parallel trials (optuna n_jobs)")
    parser.add_argument("--seed", type=int, default=None, help="TPE sampler seed, for reproducible studies")
    parser.add_argument(
        "--study-name",
        default=None,
        help="default: momentum-<timestamp>. Reuse a name (with the same --storage) to resume a study.",
    )
    parser.add_argument(
        "--storage",
        default=None,
        help="optuna storage URL, default: sqlite:///runs/optuna/<study-name>.db",
    )
    parser.add_argument(
        "--skip-build",
        action="store_true",
        help="reuse the existing target/release/xsec instead of rebuilding it first",
    )
    args = parser.parse_args(argv)

    try:
        is_start, is_end, oos_start, oos_end = split_is_oos(args.date_start, args.date_end, args.split_ratio)
    except ValueError as exc:
        _fail(str(exc))

    study_name = args.study_name or f"momentum-{datetime.now():%Y%m%d-%H%M%S}"
    STUDIES_DIR.mkdir(parents=True, exist_ok=True)
    storage = args.storage or f"sqlite:///{STUDIES_DIR / f'{study_name}.db'}"

    binary = ensure_release_binary(skip_build=args.skip_build)

    print(f"study: {study_name}")
    print(f"in-sample:     {is_start}..{is_end}")
    print(f"out-of-sample: {oos_start}..{oos_end}")

    if day_span(oos_start, oos_end) <= MAX_WARMUP_DAYS * 3:
        print(
            f"warning: the out-of-sample window is only {day_span(oos_start, oos_end)} day(s) "
            f"long. The strategy needs up to {MAX_WARMUP_DAYS} daily bars of warm-up before it "
            "trades at all (see start_universe's warm-up request in src/strategy/common.rs), so a "
            "very short window can show a flat 0% out-of-sample simply because the book barely got "
            "going — that's a too-short window, not evidence of overfitting. Widen "
            "--date-start/--date-end or lower --split-ratio if you see that.",
            file=sys.stderr,
        )

    sampler = optuna.samplers.TPESampler(seed=args.seed)
    study = optuna.create_study(
        study_name=study_name,
        storage=storage,
        direction="maximize",
        sampler=sampler,
        load_if_exists=True,
    )
    objective = make_objective(
        binary=binary,
        universe=args.universe,
        starting_balance=args.starting_balance,
        is_start=is_start,
        is_end=is_end,
        study_name=study_name,
    )
    study.optimize(objective, n_trials=args.n_trials, n_jobs=args.jobs)

    completed = [t for t in study.trials if t.state == optuna.trial.TrialState.COMPLETE]
    if not completed:
        _fail("every trial failed or was pruned; check the errors above and each trial's user_attrs['error']")

    best = study.best_trial
    print(f"\nbest trial #{best.number}: in-sample CAGR={best.value:.2%}")
    for name, value in best.params.items():
        print(f"  {name} = {value}")

    try:
        oos_value, oos_uuid = validate_oos(
            binary,
            best,
            universe=args.universe,
            starting_balance=args.starting_balance,
            oos_start=oos_start,
            oos_end=oos_end,
            study_name=study_name,
        )
    except BacktestFailed as exc:
        _fail(
            f"out-of-sample validation of best trial #{best.number} ({best.params}) failed: {exc}\n"
            f"the study itself is intact (storage: {storage}); re-run the OOS backtest by hand once "
            "the underlying issue is fixed."
        )
    print(f"\nout-of-sample CAGR={oos_value:.2%}  (run {oos_uuid})")
    gap = best.value - oos_value
    print(f"IS - OOS gap = {gap:.2%}{'  <- large gap, likely overfit' if gap > 0.15 else ''}")

    summary = write_summary(
        study_name=study_name,
        is_start=is_start,
        is_end=is_end,
        oos_start=oos_start,
        oos_end=oos_end,
        best_trial=best,
        oos_cagr=oos_value,
        oos_uuid=oos_uuid,
    )
    print(f"\nwrote {summary.relative_to(REPO_ROOT)}")
    print(f"study storage: {storage}")


if __name__ == "__main__":
    main()
