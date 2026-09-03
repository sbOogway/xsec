"""Guards the per-leg diagnostics CLI against regressions.

Run with: uv run --project analysis pytest analysis/tests/
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

ANALYSIS_DIR = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ANALYSIS_DIR))

import legs  # noqa: E402

FIXTURE_UUID = "test-fixture-0001"

LEGS_HEADER = (
    "run_id,month,instrument_id,side,entry_price,exit_price,per_leg_return,notional_usdt\n"
)


@pytest.fixture
def runs_dir(tmp_path, monkeypatch):
    """Point the CLI at a temp runs/ dir."""
    runs = tmp_path / "runs"
    runs.mkdir()
    monkeypatch.setattr(legs, "RUNS_DIR", runs)
    monkeypatch.setattr(legs, "REPO_ROOT", tmp_path)
    return runs


def write_legs(runs: Path, uuid: str, *, rows: str | None = None) -> None:
    run_dir = runs / uuid
    run_dir.mkdir(parents=True, exist_ok=True)
    if rows is None:
        # A few months, both books, a winner, a loser and a flat leg.
        rows = "".join(
            f"{uuid},2025-{m:02d},{inst},{side},100,{100 * (1 + r):.4f},{r:.6f},{notional}\n"
            for m, inst, side, r, notional in (
                (1, "BTCUSDT-LINEAR.BYBIT", "long", 0.10, 50),
                (1, "ETHUSDT-LINEAR.BYBIT", "short", 0.05, 50),
                (2, "BTCUSDT-LINEAR.BYBIT", "long", -0.08, 40),
                (2, "SOLUSDT-LINEAR.BYBIT", "short", 0.00, 40),
                (3, "ETHUSDT-LINEAR.BYBIT", "long", 0.03, 45),
                (3, "SOLUSDT-LINEAR.BYBIT", "short", -0.02, 45),
            )
        )
    (run_dir / "legs.csv").write_text(LEGS_HEADER + rows)


def _title(html: str) -> str:
    return html[html.index("<title>") + len("<title>") : html.index("</title>")]


def test_renders_self_contained_html_with_uuid_in_title(runs_dir):
    write_legs(runs_dir, FIXTURE_UUID)

    legs.main(["--uuid", FIXTURE_UUID])

    out = runs_dir / FIXTURE_UUID / "legs.html"
    assert out.exists() and out.stat().st_size > 0
    html = out.read_text()
    assert FIXTURE_UUID in _title(html)
    # self-contained: inline styles, charts as base64, nothing pulled over the wire
    assert "<style>" in html
    assert 'rel="stylesheet"' not in html
    assert 'script src="http' not in html
    assert "data:image/png;base64," in html
    for heading in (
        "Per-instrument attribution",
        "Long vs short book",
        "Leg return distribution",
        "Per-month leg breakdown",
    ):
        assert heading in html


def test_latest_picks_most_recent(runs_dir):
    import os
    import time

    write_legs(runs_dir, "older")
    write_legs(runs_dir, "newer")
    now = time.time()
    os.utime(runs_dir / "older" / "legs.csv", (now - 100, now - 100))
    os.utime(runs_dir / "newer" / "legs.csv", (now, now))

    legs.main(["--latest"])
    assert (runs_dir / "newer" / "legs.html").exists()


def test_unknown_uuid_lists_available(runs_dir, capsys):
    write_legs(runs_dir, "known-run")

    with pytest.raises(SystemExit) as exc:
        legs.main(["--uuid", "does-not-exist"])
    assert exc.value.code == 1
    assert "known-run" in capsys.readouterr().err


def test_header_only_legs_fails_loudly(runs_dir, capsys):
    write_legs(runs_dir, FIXTURE_UUID, rows="")

    with pytest.raises(SystemExit) as exc:
        legs.main(["--uuid", FIXTURE_UUID])
    assert exc.value.code == 1
    assert "no rows" in capsys.readouterr().err.lower()


def test_missing_columns_fails_loudly(runs_dir, capsys):
    run_dir = runs_dir / FIXTURE_UUID
    run_dir.mkdir(parents=True)
    (run_dir / "legs.csv").write_text("run_id,month,side\nx,2025-01,long\n")

    with pytest.raises(SystemExit) as exc:
        legs.main(["--uuid", FIXTURE_UUID])
    assert exc.value.code == 1
    assert "missing columns" in capsys.readouterr().err.lower()


def test_single_leg_run_renders(runs_dir):
    write_legs(runs_dir, FIXTURE_UUID, rows=f"{FIXTURE_UUID},2025-01,BTCUSDT-LINEAR.BYBIT,long,100,110,0.100000,50\n")

    legs.main(["--uuid", FIXTURE_UUID])
    assert (runs_dir / FIXTURE_UUID / "legs.html").stat().st_size > 0


def test_all_flat_month_renders(runs_dir):
    rows = "".join(
        f"{FIXTURE_UUID},2025-02,{inst},{side},100,100,0.000000,40\n"
        for inst, side in (
            ("BTCUSDT-LINEAR.BYBIT", "long"),
            ("ETHUSDT-LINEAR.BYBIT", "short"),
        )
    )
    write_legs(runs_dir, FIXTURE_UUID, rows=rows)

    legs.main(["--uuid", FIXTURE_UUID])
    assert (runs_dir / FIXTURE_UUID / "legs.html").stat().st_size > 0
