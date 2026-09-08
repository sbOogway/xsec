"""Guards the Optuna parameter-search CLI against regressions.

The subprocess/Rust path (`run_backtest`, `ensure_release_binary`) is exercised
manually, not here — these tests cover the pure functions (date split, CAGR)
and the objective/study plumbing with `run_backtest` stubbed out.

Run with: uv run --project analysis pytest analysis/tests/
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import optuna
import pandas as pd
import pytest

ANALYSIS_DIR = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ANALYSIS_DIR))

import optimize  # noqa: E402

PORTFOLIO_HEADER = (
    "run_id,period,period_end_date,n_long,n_short,gross_return,fee_paid_usdt,"
    "net_return,equity_end_of_period_usdt,n_fills,fills_ref\n"
)


@pytest.fixture
def runs_dir(tmp_path, monkeypatch):
    """Point the module at a temp runs/ dir."""
    runs = tmp_path / "runs"
    runs.mkdir()
    monkeypatch.setattr(optimize, "RUNS_DIR", runs)
    monkeypatch.setattr(optimize, "REPO_ROOT", tmp_path)
    monkeypatch.setattr(optimize, "STUDIES_DIR", runs / "optuna")
    return runs


def portfolio_frame(returns: list[float], *, gap_days: int = 30) -> pd.DataFrame:
    """A minimal `portfolio.csv` frame: `returns` spaced `gap_days` apart."""
    start = pd.Timestamp("2025-01-31")
    dates = [start + pd.Timedelta(days=gap_days * i) for i in range(len(returns))]
    return pd.DataFrame(
        {
            "period": [f"P{i:03d}" for i in range(len(returns))],
            "period_end_date": [d.strftime("%Y-%m-%d") for d in dates],
            "net_return": returns,
        }
    )


def write_run(runs: Path, uuid: str, *, returns: list[float], gap_days: int = 30) -> None:
    run_dir = runs / uuid
    run_dir.mkdir(parents=True, exist_ok=True)
    frame = portfolio_frame(returns, gap_days=gap_days)
    rows = "".join(
        f"{uuid},{row.period},{row.period_end_date},5,5,{row.net_return:.6f},1.0,"
        f"{row.net_return:.6f},{1000 * (1 + row.net_return):.2f},10,runs/{uuid}/fills.csv\n"
        for row in frame.itertuples()
    )
    (run_dir / "portfolio.csv").write_text(PORTFOLIO_HEADER + rows)


# -- split_is_oos -------------------------------------------------------


def test_split_default_ratio_is_month_aligned():
    is_start, is_end, oos_start, oos_end = optimize.split_is_oos("2020-01-01", "2020-12-31")
    assert (is_start, is_end, oos_start, oos_end) == (
        "2020-01-01",
        "2020-08-31",
        "2020-09-01",
        "2020-12-31",
    )


def test_split_custom_ratio():
    _, is_end, oos_start, _ = optimize.split_is_oos("2020-01-01", "2020-12-31", ratio=0.5)
    assert (is_end, oos_start) == ("2020-06-30", "2020-07-01")


def test_split_always_leaves_at_least_one_oos_month():
    _, is_end, oos_start, oos_end = optimize.split_is_oos("2020-01-01", "2020-02-29", ratio=0.99)
    assert oos_start <= oos_end
    assert is_end < oos_start


def test_split_too_short_range_raises():
    with pytest.raises(ValueError, match="need at least 2"):
        optimize.split_is_oos("2020-01-01", "2020-01-15")


def test_split_reversed_dates_raises():
    with pytest.raises(ValueError, match="must be before"):
        optimize.split_is_oos("2020-06-01", "2020-01-01")


def test_split_bad_ratio_raises():
    with pytest.raises(ValueError, match="ratio must be"):
        optimize.split_is_oos("2020-01-01", "2020-12-31", ratio=1.5)


def test_day_span():
    assert optimize.day_span("2020-01-01", "2020-01-01") == 1
    assert optimize.day_span("2020-01-01", "2020-12-31") == 366  # 2020 is a leap year


# -- cagr -----------------------------------------------------------------


def test_cagr_annualizes_by_calendar_span():
    # three daily periods, +1% each: growth 1.030301 over 3/365.25 years.
    frame = portfolio_frame([0.01, 0.01, 0.01], gap_days=1)
    years = 3 * 1 / 365.25
    assert optimize.cagr(frame) == pytest.approx((1.01**3) ** (1.0 / years) - 1.0)


def test_cagr_monthly_cadence_matches_month_count():
    # 12 periods spaced 30 days: ~360/365.25 years, i.e. roughly one year.
    frame = portfolio_frame([0.02] * 12, gap_days=30)
    years = 12 * 30 / 365.25
    assert optimize.cagr(frame) == pytest.approx((1.02**12) ** (1.0 / years) - 1.0)


def test_cagr_wipeout_floors_at_minus_one():
    frame = portfolio_frame([-1.0, 0.5], gap_days=30)
    assert optimize.cagr(frame) == -1.0


def test_cagr_empty_raises():
    with pytest.raises(ValueError):
        optimize.cagr(portfolio_frame([]))


# -- params_to_argv ---------------------------------------------------------


def test_params_to_argv_formats_int_and_float():
    argv = optimize.params_to_argv({"fast_days": 3, "top_n": 5, "long_w": 0.5})
    assert argv == [
        "--fast-days",
        "3",
        "--top-n",
        "5",
        "--long-w",
        "0.500000",
    ]


def test_suggest_params_covers_the_search_space():
    study = optuna.create_study()
    params = optimize.suggest_params(study.ask())
    assert set(params) == set(optimize.SEARCH_SPACE)
    for name, value in params.items():
        kind, low, high = optimize.SEARCH_SPACE[name]
        assert low <= value <= high
        if kind == "int":
            assert isinstance(value, int)


# -- load_portfolio ---------------------------------------------------


def test_load_portfolio_reads_sorted_frame(runs_dir):
    write_run(runs_dir, "r1", returns=[0.01, -0.02, 0.03])
    frame = optimize.load_portfolio("r1")
    assert list(frame["net_return"].round(6)) == [0.01, -0.02, 0.03]
    assert list(frame["period"]) == ["P000", "P001", "P002"]


def test_load_portfolio_missing_run_fails(runs_dir):
    with pytest.raises(optimize.BacktestFailed):
        optimize.load_portfolio("nope")


def test_load_portfolio_header_only_fails(runs_dir):
    run_dir = runs_dir / "r1"
    run_dir.mkdir()
    (run_dir / "portfolio.csv").write_text(PORTFOLIO_HEADER)
    with pytest.raises(optimize.BacktestFailed):
        optimize.load_portfolio("r1")


# -- objective ------------------------------------------------------------


def test_objective_success_returns_cagr_and_records_attrs(runs_dir, monkeypatch):
    def fake_run_backtest(binary, uuid, **kwargs):
        write_run(runs_dir, uuid, returns=[0.02, 0.02, 0.02], gap_days=30)

    monkeypatch.setattr(optimize, "run_backtest", fake_run_backtest)

    objective = optimize.make_objective(
        binary=Path("fake-binary"),
        universe="universe.txt",
        starting_balance="1_000 USDT",
        is_start="2020-01-01",
        is_end="2020-03-31",
        study_name="test-study",
    )
    study = optuna.create_study(direction="maximize")
    study.optimize(objective, n_trials=1)

    trial = study.trials[0]
    years = 3 * 30 / 365.25
    assert trial.state == optuna.trial.TrialState.COMPLETE
    assert trial.value == pytest.approx((1.02**3) ** (1.0 / years) - 1.0)
    assert trial.user_attrs["run_uuid"] == "test-study-trial0000"
    assert trial.user_attrs["n_periods"] == 3


def test_objective_prunes_on_backtest_failure(runs_dir, monkeypatch):
    def fake_run_backtest(binary, uuid, **kwargs):
        raise optimize.BacktestFailed("bad params")

    monkeypatch.setattr(optimize, "run_backtest", fake_run_backtest)

    objective = optimize.make_objective(
        binary=Path("fake-binary"),
        universe="universe.txt",
        starting_balance="1_000 USDT",
        is_start="2020-01-01",
        is_end="2020-03-31",
        study_name="test-study",
    )
    study = optuna.create_study(direction="maximize")
    study.optimize(objective, n_trials=1)

    trial = study.trials[0]
    assert trial.state == optuna.trial.TrialState.PRUNED
    assert "bad params" in trial.user_attrs["error"]


# -- main -------------------------------------------------------------------


def test_main_runs_end_to_end_and_writes_summary(runs_dir, monkeypatch, capsys):
    monkeypatch.setattr(optimize, "ensure_release_binary", lambda *, skip_build: Path("fake-binary"))

    def fake_run_backtest(binary, uuid, **kwargs):
        write_run(runs_dir, uuid, returns=[0.01, 0.015, 0.02], gap_days=30)

    monkeypatch.setattr(optimize, "run_backtest", fake_run_backtest)

    optimize.main(
        [
            "--date-start",
            "2020-01-01",
            "--date-end",
            "2020-06-30",
            "--n-trials",
            "2",
            "--study-name",
            "smoke-study",
        ]
    )

    captured = capsys.readouterr()
    assert "best trial" in captured.out
    assert "out-of-sample CAGR" in captured.out
    # the 2020-01-01..2020-06-30 window's OOS slice is short enough to trip
    # the too-short-for-warm-up warning.
    assert "warning: the out-of-sample window" in captured.err

    summary_path = runs_dir / "optuna" / "smoke-study.json"
    assert summary_path.exists()
    payload = json.loads(summary_path.read_text())
    assert payload["study_name"] == "smoke-study"
    assert payload["oos_run_uuid"] == "smoke-study-oos-best"
    assert (runs_dir / "smoke-study-oos-best" / "portfolio.csv").exists()
