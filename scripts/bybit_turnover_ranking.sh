#!/usr/bin/env bash
# Rank Bybit linear perpetuals by 24h USD turnover (dollar volume). Bybit's
# category=linear tickers mix crypto with stock/ETF/commodity perpetuals,
# distinguished by the instruments-info "symbolType" field (crypto is "";
# non-crypto is "stock", "ETF", "commodity", ...). By default this script
# keeps only symbolType == "" (crypto); pass --all to keep everything.
#
# Usage:
#   scripts/bybit_turnover_ranking.sh       # crypto only, ascending turnover
#   scripts/bybit_turnover_ranking.sh --all # include stocks/ETFs/commodities
set -euo pipefail

include_all=0
if [[ "${1:-}" == "--all" ]]; then
  include_all=1
fi

tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT

instruments_file="$tmp_dir/instruments.json"
cursor=""
first=1

# instruments-info paginates at 1000/page; loop until nextPageCursor is empty.
: > "$instruments_file"
while true; do
  url="https://api.bybit.com/v5/market/instruments-info?category=linear&limit=1000"
  if [[ -n "$cursor" ]]; then
    url="${url}&cursor=${cursor}"
  fi
  page=$(curl -s "$url")
  jq -c '.result.list[]' <<<"$page" >> "$instruments_file"
  cursor=$(jq -r '.result.nextPageCursor' <<<"$page")
  if [[ -z "$cursor" ]]; then
    break
  fi
done

if [[ "$include_all" -eq 1 ]]; then
  symbol_filter='true'
else
  symbol_filter='.symbolType == ""'
fi

jq -r "select(${symbol_filter}) | .symbol" "$instruments_file" > "$tmp_dir/symbols.txt"

curl -s "https://api.bybit.com/v5/market/tickers?category=linear" \
  | jq -r --slurpfile symbols <(jq -Rn '[inputs]' "$tmp_dir/symbols.txt") '
      .result.list
      | map(select(.symbol as $s | $symbols[0] | index($s)))
      | sort_by(.turnover24h | tonumber)
      | .[]
      | "\(.symbol): \(.turnover24h)"
    '
