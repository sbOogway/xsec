#!/usr/bin/env bash
# Rank the largest crypto assets by 24h USD volume using the CoinGecko API, in
# the same "SYMBOL: value" ascending format as bybit_turnover_ranking.sh, so the
# same `... | tac | cut -d':' -f1 | head -N` pipeline builds a top-N universe.
#
# CoinGecko aggregates spot markets (not perps), so the raw list is full of
# stablecoins and wrapped/staked tokens. By default those are dropped; pass
# --all to keep everything (mirrors bybit_turnover_ranking.sh --all).
#
# Ranking metric: --by volume (default, matches Bybit turnover) or --by mcap.
# Depth: --pages N fetches N pages of 250 (default 1 == top 250 by the metric).
#
# Optional: set COINGECKO_API_KEY to send a CoinGecko demo/pro key (higher rate
# limits); without it the script uses the keyless public endpoint.
#
# Usage:
#   scripts/coingecko_top_ranking.sh              # crypto only, ascending 24h volume
#   scripts/coingecko_top_ranking.sh --all        # include stablecoins / wrapped tokens
#   scripts/coingecko_top_ranking.sh --by mcap    # rank by market cap instead
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exclude_file="${script_dir}/excluded_assets.txt"

include_all=0
metric="volume"      # "volume" | "mcap"
pages=1

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
    --pages)
      shift
      pages="${1:-}"
      [[ "$pages" =~ ^[0-9]+$ && "$pages" -ge 1 ]] || { echo "--pages wants a positive integer" >&2; exit 2; }
      ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
  shift
done

if [[ "$metric" == "mcap" ]]; then
  order="market_cap_desc"
else
  order="volume_desc"
fi

base="https://api.coingecko.com/api/v3"
key_header=()
if [[ -n "${COINGECKO_API_KEY:-}" ]]; then
  # Pro keys use api.coingecko.com's /pro subdomain; demo keys use the public
  # host with an x-cg-demo-api-key header. The demo header is harmless on the
  # public host, so send that and leave the host alone.
  key_header=(-H "x-cg-demo-api-key: ${COINGECKO_API_KEY}")
fi

tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT
markets_file="$tmp_dir/markets.json"
: > "$markets_file"

for ((page = 1; page <= pages; page++)); do
  url="${base}/coins/markets?vs_currency=usd&order=${order}&per_page=250&page=${page}&sparkline=false"
  resp=$(curl -s "${key_header[@]}" "$url")
  if ! jq -e 'type == "array"' <<<"$resp" >/dev/null 2>&1; then
    echo "CoinGecko request failed (page ${page}):" >&2
    jq -r '.status.error_message // .error // .' <<<"$resp" >&2 || echo "$resp" >&2
    exit 1
  fi
  jq -c '.[]' <<<"$resp" >> "$markets_file"
done

# Stablecoins and wrapped / staked derivatives to drop (unless --all); see
# scripts/excluded_assets.txt for the list and the rationale.
deny=$(sed 's/#.*//' "$exclude_file" | tr -d '[:blank:]' | grep -v '^$' | jq -Rn '[inputs | ascii_upcase]')

jq -rs \
  --argjson deny "$deny" \
  --argjson keep_all "$include_all" \
  --arg metric "$metric" '
    map({
      sym:  (.symbol | ascii_upcase),
      vol:  .total_volume,
      mcap: .market_cap
    })
    | map(select(.vol != null and .mcap != null))
    | map(select($keep_all == 1 or ((.sym) as $s | ($deny | index($s)) | not)))
    | unique_by(.sym)
    | map(. + { rank: (if $metric == "mcap" then .mcap else .vol end) })
    | sort_by(.rank)
    | .[]
    | "\(.sym): \(.rank)"
  ' "$markets_file"
