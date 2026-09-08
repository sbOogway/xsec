#!/usr/bin/env bash
# Rank backtest runs of the `momentum` strategy by a chosen tearsheet metric.
#
# Scans runs/*/config.csv for exactly `strategy,momentum` (so cross_sectional_
# momentum and top5_momentum_filtered runs are excluded), pulls the headline
# numbers out of each run's tearsheet.html, and prints them as one table sorted
# by --by (default Sharpe). The run id is the runs/<id>/ directory that holds
# the full tearsheet, fills, legs and portfolio CSVs.
#
# Metric columns: CAGR, Sharpe, Sortino, Calmar, MaxDD (max drawdown, %),
# Vol (annualised, %), CumRet (cumulative return, %). Then the params that
# distinguish momentum runs: win=fast/medium/slow signal days,
# w=fast/medium/slow signal weights, tn/sn=top_n/short_n, lsb=long_short_balance
# (older runs call it long_w), regime=regime_filter, risk=risk_fraction,
# hold=number_holding_periods x holding_period.
#
# Usage:
#   scripts/momentum_run_ranking.sh                # sorted by Sharpe, best first
#   scripts/momentum_run_ranking.sh --by cagr      # cagr | sharpe | sortino |
#                                                  # calmar | maxdd | vol | cumret
#   scripts/momentum_run_ranking.sh --by maxdd     # shallowest drawdown first
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
runs_dir="$(cd "$script_dir/.." && pwd)/runs"

by="sharpe"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --by) by="${2:-}"; shift 2 ;;
    -h|--help) grep '^#' "$0" | cut -c3- ; exit 0 ;;
    *) echo "unknown arg: $1 (see --help)" >&2; exit 2 ;;
  esac
done

# Which TSV column (1-based) each metric lands in, per the printf below.
case "${by,,}" in
  cagr)    sort_col=3 ;;
  sharpe)  sort_col=4 ;;
  sortino) sort_col=5 ;;
  calmar)  sort_col=6 ;;
  maxdd)   sort_col=7 ;;   # -9.1 sorts above -32.2, i.e. shallowest first
  vol)     sort_col=8 ;;
  cumret)  sort_col=9 ;;
  *) echo "unknown --by metric: $by (want cagr|sharpe|sortino|calmar|maxdd|vol|cumret)" >&2; exit 2 ;;
esac

# metric <label-regex> <tearsheet> -> the row's number, commas and % stripped.
metric() {
  grep -oP "(?<=<td>$1</td><td>)-?[\d,]+(?:\.\d+)?" "$2" | head -1 | tr -d ','
}

# cfg <key> <config.csv> -> the value column (everything after the first comma).
cfg() {
  grep -m1 "^$1," "$2" | cut -d, -f2-
}

emit_rows() {
  for cfg_file in "$runs_dir"/*/config.csv; do
    [[ -f "$cfg_file" ]] || continue
    grep -qx 'strategy,momentum' "$cfg_file" || continue
    dir="$(dirname "$cfg_file")"
    ts="$dir/tearsheet.html"
    [[ -f "$ts" ]] || continue

    run="$(basename "$dir")"
    span="$(cfg date_start "$cfg_file")..$(cfg date_end "$cfg_file")"

    cagr="$(metric 'CAGR﹪' "$ts")"
    sharpe="$(metric 'Sharpe' "$ts")"
    sortino="$(metric 'Sortino' "$ts")"
    calmar="$(metric 'Calmar' "$ts")"
    maxdd="$(metric 'Max Drawdown' "$ts")"
    vol="$(metric 'Volatility \(ann\.\)' "$ts")"
    cumret="$(metric 'Cumulative Return' "$ts")"

    lsb="$(cfg long_short_balance "$cfg_file")"
    [[ -z "$lsb" ]] && lsb="$(cfg long_w "$cfg_file")"
    params="win=$(cfg fast_days "$cfg_file")/$(cfg medium_days "$cfg_file")/$(cfg slow_days "$cfg_file")"
    params+=" w=$(cfg fast_weight "$cfg_file")/$(cfg medium_weight "$cfg_file")/$(cfg slow_weight "$cfg_file")"
    params+=" tn=$(cfg top_n "$cfg_file") sn=$(cfg short_n "$cfg_file") lsb=${lsb}"
    params+=" regime=$(cfg regime_filter "$cfg_file") risk=$(cfg risk_fraction "$cfg_file")"
    params+=" hold=$(cfg number_holding_periods "$cfg_file")x$(cfg holding_period "$cfg_file")"

    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
      "$run" "$span" "${cagr:-NA}" "${sharpe:-NA}" "${sortino:-NA}" \
      "${calmar:-NA}" "${maxdd:-NA}" "${vol:-NA}" "${cumret:-NA}" "$params"
  done
}

rows="$(emit_rows)"
if [[ -z "$rows" ]]; then
  echo "no momentum-strategy runs found under $runs_dir" >&2
  exit 1
fi

{
  printf 'run\tspan\tCAGR\tSharpe\tSortino\tCalmar\tMaxDD\tVol\tCumRet\tparams\n'
  # -g: numeric sort that understands the signs and decimals; -r: best first.
  # NA rows sort to the bottom either way.
  sort -t$'\t' -k"${sort_col},${sort_col}" -g -r <<<"$rows"
} | column -t -s$'\t'
