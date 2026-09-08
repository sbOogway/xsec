#!/usr/bin/env bash
# Rank the largest crypto assets by 24h USD volume using the CoinMarketCap API,
# in the same "SYMBOL: value" ascending format as bybit_turnover_ranking.sh, so
# the same `... | tac | cut -d':' -f1 | head -N` pipeline builds a top-N universe.
#
# CMC's listings are spot-market aggregates, so the raw list is full of
# stablecoins and wrapped/staked tokens. By default those are dropped; pass
# --all to keep everything (mirrors bybit_turnover_ranking.sh --all).
#
# Ranking metric: --by volume (default, matches Bybit turnover) or --by mcap.
# Depth: --limit N pulls the top N rows by the metric (default 200, max 5000;
# note larger pulls cost more API credits).
#
# Requires: CMC_API_KEY in the environment (a free CoinMarketCap "Basic" key
# works). Get one at https://coinmarketcap.com/api/.
#
# Usage:
#   CMC_API_KEY=... scripts/coinmarketcap_top_ranking.sh           # crypto only, asc 24h volume
#   CMC_API_KEY=... scripts/coinmarketcap_top_ranking.sh --all     # include stablecoins / wrapped
#   CMC_API_KEY=... scripts/coinmarketcap_top_ranking.sh --by mcap # rank by market cap instead
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exclude_file="${script_dir}/excluded_assets.txt"

if [[ -z "${CMC_API_KEY:-}" ]]; then
  echo "CMC_API_KEY is not set (get a free key at https://coinmarketcap.com/api/)" >&2
  exit 1
fi

include_all=0
metric="volume"      # "volume" | "mcap"
limit=200

while [[ $# -gt 0 ]]; do
  case "$1" in
    --all) include_all=1 ;;
    --by)
      shift
      case "${1:-}" in
        volume|vol) metric="volume" ;;
        mcap|market-cap|marketcap) metric="mcap" ;;
        *) echo "unknown --by value: ${1:-}" >&2; exit 2 ;;
      esac
      ;;
    --limit)
      shift
      limit="${1:-}"
      [[ "$limit" =~ ^[0-9]+$ && "$limit" -ge 1 && "$limit" -le 5000 ]] \
        || { echo "--limit wants an integer in 1..5000" >&2; exit 2; }
      ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
  shift
done

if [[ "$metric" == "mcap" ]]; then
  sort_key="market_cap"
else
  sort_key="volume_24h"
fi

url="https://pro-api.coinmarketcap.com/v1/cryptocurrency/listings/latest?start=1&limit=${limit}&sort=${sort_key}&sort_dir=desc&convert=USD"
resp=$(curl -s -H "X-CMC_PRO_API_KEY: ${CMC_API_KEY}" -H "Accept: application/json" "$url")

if [[ "$(jq -r '.status.error_code // 0' <<<"$resp")" != "0" ]]; then
  echo "CoinMarketCap request failed:" >&2
  jq -r '.status.error_message // .' <<<"$resp" >&2
  exit 1
fi

# Stablecoins and wrapped / staked derivatives to drop (unless --all); see
# scripts/excluded_assets.txt for the list and the rationale.
deny=$(sed 's/#.*//' "$exclude_file" | tr -d '[:blank:]' | grep -v '^$' | jq -Rn '[inputs | ascii_upcase]')

jq -r \
  --argjson deny "$deny" \
  --argjson keep_all "$include_all" \
  --arg metric "$metric" '
    .data
    | map({
        sym:  (.symbol | ascii_upcase),
        vol:  .quote.USD.volume_24h,
        mcap: .quote.USD.market_cap
      })
    | map(select(.vol != null and .mcap != null))
    | map(select($keep_all == 1 or ((.sym) as $s | ($deny | index($s)) | not)))
    | unique_by(.sym)
    | map(. + { rank: (if $metric == "mcap" then .mcap else .vol end) })
    | sort_by(.rank)
    | .[]
    | "\(.sym): \(.rank)"
  ' <<<"$resp"
