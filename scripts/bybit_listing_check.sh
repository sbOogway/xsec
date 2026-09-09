#!/usr/bin/env bash
# Given a list of base-asset symbols on stdin (or in files passed as args) — the
# kind `coingecko_top_ranking.sh | ... | cut -d: -f1` emits, or a
# coins/*_top_*.txt universe file — report which ones have a USDT-margined
# linear *perpetual* trading on Bybit (dated futures don't count).
#
# Matching is best-effort:
#   * contract multipliers are normalised away, prefix or suffix
#     (1000PEPE, 10000SATS, SHIB1000  <->  PEPE, SATS, SHIB);
#   * a small hand-maintained alias map covers well-known renames
#     (MATIC->POL, PUMP->PUMPFUN). Extend ALIASES below as needed.
#
# Output (input order):
#   --report   (default)  "SYMBOL: listed (1000PEPEUSDT)" / "SYMBOL: missing"
#   --listed              the Bybit base coin of each listed name (PEPE -> 1000PEPE,
#                         SHIB -> SHIB1000) — drop straight into a universe file
#   --missing             the missing input symbols only, bare
#   --annotate            every input symbol, in order, as a universe file: a
#                         tradeable name as its Bybit base coin, a stablecoin or
#                         a name with no Bybit perp commented out with the reason
#   --map                 every input symbol as "SYMBOL,BYBIT_BASE,STATUS" —
#                         STATUS is listed|stablecoin|unlisted, BYBIT_BASE empty
#                         for the latter two. A machine-readable resolution table
#                         (used by `momentum --source coinmarketcap`).
# A "listed N / stablecoin S / unlisted U / total M" summary goes to stderr.
#
# Usage:
#   scripts/coingecko_top_ranking.sh | tac | cut -d: -f1 | scripts/bybit_listing_check.sh
#   scripts/coingecko_top_ranking.sh --all | tac | cut -d: -f1 | scripts/bybit_listing_check.sh --annotate
#   scripts/bybit_listing_check.sh --listed coins/coingecko_top_100_2026-09-07*.txt
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exclude_file="${script_dir}/excluded_assets.txt"

mode="report"
files=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --report)   mode="report" ;;
    --listed)   mode="listed" ;;
    --missing)  mode="missing" ;;
    --annotate) mode="annotate" ;;
    --map)      mode="map" ;;
    -*) echo "unknown argument: $1" >&2; exit 2 ;;
    *) files+=("$1") ;;
  esac
  shift
done

# CoinGecko/CMC ticker  ->  Bybit baseCoin, for pure renames the multiplier
# normalisation can't catch. One direction (input symbol -> Bybit).
declare -A ALIASES=(
  [MATIC]=POL
  [PUMP]=PUMPFUN
  [MIOTA]=IOTA
  [NANO]=XNO
)

# Stablecoins and wrapped / staked derivatives — commented out (with a reason)
# rather than dropped in --annotate mode. See scripts/excluded_assets.txt.
declare -A EXCLUDED
while IFS= read -r sym; do
  [[ -n "$sym" ]] && EXCLUDED["$sym"]=1
done < <(sed 's/#.*//' "$exclude_file" | tr -d '[:blank:]' | grep -v '^$' | tr '[:lower:]' '[:upper:]')

mult='(1000000|100000|10000|1000)'
norm() {
  local s="${1^^}"
  [[ "$s" =~ ^${mult}(.+)$ ]] && s="${BASH_REMATCH[2]}"
  [[ "$s" =~ ^(.+)${mult}$ ]] && s="${BASH_REMATCH[1]}"
  printf '%s' "$s"
}

# Read input symbols: strip a trailing "# ..." comment or ": value" suffix, drop
# whitespace, upper-case, keep first-seen order.
declare -A seen
symbols=()
while IFS= read -r line || [[ -n "$line" ]]; do
  line="${line%%#*}"
  line="${line%%:*}"
  line="${line//[[:space:]]/}"
  line="${line^^}"
  [[ -z "$line" ]] && continue
  [[ -n "${seen[$line]:-}" ]] && continue
  seen["$line"]=1
  symbols+=("$line")
done < <(cat -- "${files[@]:-/dev/stdin}")

if [[ ${#symbols[@]} -eq 0 ]]; then
  echo "no input symbols" >&2
  exit 1
fi

tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT
instruments="$tmp_dir/instruments.json"
: > "$instruments"

# instruments-info paginates at 1000/page; loop until nextPageCursor is empty.
cursor=""
while true; do
  url="https://api.bybit.com/v5/market/instruments-info?category=linear&limit=1000"
  [[ -n "$cursor" ]] && url="${url}&cursor=${cursor}"
  page=$(curl -s "$url")
  if [[ "$(jq -r '.retCode' <<<"$page")" != "0" ]]; then
    echo "Bybit instruments-info failed: $(jq -r '.retMsg // "unknown error"' <<<"$page")" >&2
    exit 1
  fi
  jq -c '.result.list[]' <<<"$page" >> "$instruments"
  cursor=$(jq -r '.result.nextPageCursor' <<<"$page")
  [[ -z "$cursor" || "$cursor" == "null" ]] && break
done

# Map every crypto USDT *perpetual* that is trading to its Bybit symbol, keyed by
# both the raw baseCoin and its multiplier-normalised form.
declare -A listed_symbol
while IFS=$'\t' read -r key sym; do
  [[ ${#key} -ge 2 ]] && listed_symbol["$key"]="$sym"
done < <(jq -r --arg m "$mult" '
  select(.symbolType == "" and .quoteCoin == "USDT"
         and .contractType == "LinearPerpetual" and .status == "Trading")
  | (.baseCoin | ascii_upcase) as $b
  | .symbol as $s
  | ($b | sub("^" + $m; "") | sub($m + "$"; "")) as $n
  | "\($b)\t\($s)", "\($n)\t\($s)"
' "$instruments")

lookup() {
  local s="$1" k
  for k in "$s" "$(norm "$s")" "${ALIASES[$s]:-}"; do
    if [[ -n "$k" && -n "${listed_symbol[$k]:-}" ]]; then
      printf '%s' "${listed_symbol[$k]}"
      return 0
    fi
  done
  return 0
}

n_listed=0
n_stable=0
n_unlisted=0
for s in "${symbols[@]}"; do
  if [[ ( "$mode" == "annotate" || "$mode" == "map" ) && -n "${EXCLUDED[$s]:-}" ]]; then
    n_stable=$((n_stable + 1))
    case "$mode" in
      annotate) echo "# ${s}   # stablecoin / derivative" ;;
      map)      echo "${s},,stablecoin" ;;
    esac
    continue
  fi

  match="$(lookup "$s")"
  if [[ -n "$match" ]]; then
    n_listed=$((n_listed + 1))
    case "$mode" in
      report)          echo "$s: listed ($match)" ;;
      listed|annotate) echo "${match%USDT}" ;;
      map)             echo "${s},${match%USDT},listed" ;;
    esac
  else
    n_unlisted=$((n_unlisted + 1))
    case "$mode" in
      report)   echo "$s: missing" ;;
      missing)  echo "$s" ;;
      annotate) echo "# ${s}   # no Bybit USDT perp" ;;
      map)      echo "${s},,unlisted" ;;
    esac
  fi
done

if [[ "$mode" == "annotate" || "$mode" == "map" ]]; then
  echo "listed ${n_listed} / stablecoin ${n_stable} / unlisted ${n_unlisted} / total ${#symbols[@]}" >&2
else
  echo "listed ${n_listed} / total ${#symbols[@]}" >&2
fi
