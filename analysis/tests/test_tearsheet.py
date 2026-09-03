"""Guards the tearsheet CLI against regressions.

Run with: uv run pytest analysis/tests/
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

ANALYSIS_DIR = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ANALYSIS_DIR))

import tearsheet  # noqa: E402

FIXTURE_UUID = "test-fixture-0001"


@pytest.fixture
def runs_dir(tmp_path, monkeypatch):
    """Point the CLI at a temp runs/ dir with one synthetic run."""
    runs = tmp_path / "runs"
    runs.mkdir()
    monkeypatch.setattr(tearsheet, "RUNS_DIR", runs)
    monkeypatch.setattr(tearsheet, "REPO_ROOT", tmp_path)
    return runs


def write_run(runs: Path, uuid: str, *, months: int = 8, empty_portfolio: bool = False) -> None:
    portfolio = runs / f"{uuid}.portfolio.csv"
    legs = runs / f"{uuid}.legs.csv"

    legs.write_text(
        "run_id,month,instrument_id,side,entry_bar_open,exit_bar_close,per_leg_return,notional_usdt\n"
        f"{uuid},2025-01,BTCUSDT-LINEAR.BYBIT,long,100,110,0.100000,50\n"
    )

    header = (
        "run_id,month,n_long,n_short,gross_return,fee_paid_usdt,"
        "net_return,equity_end_of_month_usdt,n_fills,fills_ref\n"
    )
    rows = "" if empty_portfolio else "".join(
        f"{uuid},2025-{m:02d},5,5,0.02,1.0,0.019,{1000 + m}, 10,runs/{uuid}.fills.csv\n"
        for m in range(1, months + 1)
    )
    portfolio.write_text(header + rows)


def test_renders_html_with_uuid_in_title(runs_dir):
    write_run(runs_dir, FIXTURE_UUID)

    tearsheet.main(["--uuid", FIXTURE_UUID])

    out = runs_dir / f"{FIXTURE_UUID}.tearsheet.html"
    assert out.exists() and out.stat().st_size > 0
    html = out.read_text()
    title = html[html.index("<title>") + len("<title>") : html.index("</title>")]
    assert FIXTURE_UUID in title
    # self-contained: styles are inlined and no external scripts are pulled
    # (QuantStats links only a favicon; charts are embedded as base64 SVG).
    assert "<style>" in html
    assert 'rel="stylesheet"' not in html
    assert 'script src="http' not in html


def test_latest_picks_most_recent(runs_dir):
    write_run(runs_dir, "older")
    write_run(runs_dir, "newer")
    # make "newer" win the mtime race deterministically
    import os
    import time

    now = time.time()
    os.utime(runs_dir / "older.legs.csv", (now - 100, now - 100))
    os.utime(runs_dir / "newer.legs.csv", (now, now))

    tearsheet.main(["--latest"])
    assert (runs_dir / "newer.tearsheet.html").exists()


def test_unknown_uuid_lists_available(runs_dir, capsys):
    write_run(runs_dir, "known-run")

    with pytest.raises(SystemExit) as exc:
        tearsheet.main(["--uuid", "does-not-exist"])
    assert exc.value.code == 1
    assert "known-run" in capsys.readouterr().err


def test_empty_portfolio_fails_loudly(runs_dir, capsys):
    write_run(runs_dir, FIXTURE_UUID, empty_portfolio=True)

    with pytest.raises(SystemExit) as exc:
        tearsheet.main(["--uuid", FIXTURE_UUID])
    assert exc.value.code == 1
    assert "empty" in capsys.readouterr().err.lower()
