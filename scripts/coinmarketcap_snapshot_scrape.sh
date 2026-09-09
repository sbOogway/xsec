#!/usr/bin/env bash
# Scrape CoinMarketCap's historical market-cap rankings into one CSV per day, for
# building a point-in-time "top N by market cap" universe that rebalances as
# coins enter and leave the top N (consumed by `momentum --source coinmarketcap`).
#
# Transport: the keyless public data-api the coinmarketcap.com/historical/ pages
# call under the hood —
#   GET https://api.coinmarketcap.com/data-api/v3/cryptocurrency/listings/historical
#       ?date=<YYYY-MM-DD>&start=1&limit=<N>&convert=USD
# No CMC_API_KEY needed (the pro API's listings/historical is a paid add-on). It
# serves *daily* history — not just the Sunday snapshots the website shows —
# roughly 2013-04-28 .. yesterday. A date outside that range, or with no data,
# comes back HTTP 200 with status.error_code != "0" (CMC sometimes phrases it as
# a misleading "The system is busy"); those are logged and skipped, not fatal.
#
# CMC's historical data occasionally has holes: a handful of snapshots return
# fewer than --limit rows, with gaps at some ranks (coins pulled from the
# historical listing after the fact). That is faithful to the source, not a
# scrape bug — the row is flagged `~ N/limit` and the file is kept. The
# `momentum --source coinmarketcap` consumer already intersects each snapshot
# with the Bybit manifest, so a few missing mid-ranks don't matter.
#
# If CMC starts rate-limiting a long run, pass --tor to route every request
# through Tor (needs `torsocks` on PATH; do not also wrap the whole script in
# `torsocks`). The script also flips to torsocks on its own if it sees an
# HTTP 429 / 403 mid-run and torsocks is available.
#
# Output: coins/cmc/<YYYYMMDD>.csv, one row per coin, ranked by market cap:
#   rank,cmc_id,symbol,name,market_cap_usd,price_usd,volume_24h_usd,
#   pct_change_1h,pct_change_24h,pct_change_7d
# price / market cap / volume are USD. pct_change_1h / 24h / 7d are CMC's
# trailing price returns as of the snapshot date, in percent (pct_change_7d is
# the trailing-week return) — empty on a few pre-2014 rows. cmc_id is CMC's
# stable numeric coin id; it disambiguates tickers CMC has re-used across the
# years (two different coins both "UNI"), which the raw symbol can't.
# The raw CMC symbol is kept verbatim — no multiplier / alias normalisation, no
# stablecoin filtering. That is bybit_listing_check.sh's job, applied downstream
# in `make universe_cmc_union`, exactly as the CoinGecko pipeline does it.
#
# coins/cmc/ is a regenerable local cache (gitignored, like data/) — ~2400 files
# for the 2020-onwards default. `make universe_cmc_union` flattens a date range
# of it into the committed universe file the strategy actually reads.
#
# Idempotent and resumable: an existing <YYYYMMDD>.csv is left alone unless
# --refresh. The run ends with `N fetched / M skipped / K failed` on stderr.
#
# Usage:
#   scripts/coinmarketcap_snapshot_scrape.sh                        # 2020-01-01..today, top 200
#   scripts/coinmarketcap_snapshot_scrape.sh --from 2024-01-01 --to 2024-02-01
#   scripts/coinmarketcap_snapshot_scrape.sh --limit 300 --sleep 0.5
#   scripts/coinmarketcap_snapshot_scrape.sh --refresh --from 2024-01-07 --to 2024-01-07
#   scripts/coinmarketcap_snapshot_scrape.sh --tor                  # every request via Tor
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"

from="2020-01-01"
to="$(date -u +%F)"
limit=200
sleep_s=1
refresh=0
use_tor=0
out_dir="$repo_root/coins/cmc"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --from)    shift; from="${1:-}" ;;
    --to)      shift; to="${1:-}" ;;
    --limit)   shift; limit="${1:-}" ;;
    --sleep)   shift; sleep_s="${1:-}" ;;
    --refresh) refresh=1 ;;
    --tor)     use_tor=1 ;;
    -h|--help) grep '^#' "$0" | cut -c3- ; exit 0 ;;
    *) echo "unknown argument: $1 (see --help)" >&2; exit 2 ;;
  esac
  shift || true   # a value flag already consumed its argument
done

# `date -d ""` resolves to "now" on GNU coreutils, so guard empty values (a bare
# trailing `--from` / `--to`) explicitly before the date parse.
[[ -n "$from" ]] || { echo "--from needs a YYYY-MM-DD value" >&2; exit 2; }
[[ -n "$to"   ]] || { echo "--to needs a YYYY-MM-DD value" >&2; exit 2; }
date -d "$from" +%F >/dev/null 2>&1 || { echo "--from: not a date: $from" >&2; exit 2; }
date -d "$to"   +%F >/dev/null 2>&1 || { echo "--to: not a date: $to" >&2; exit 2; }
[[ "$limit" =~ ^[0-9]+$ && "$limit" -ge 1 && "$limit" -le 5000 ]] \
  || { echo "--limit wants an integer in 1..5000" >&2; exit 2; }
[[ "$sleep_s" =~ ^[0-9]+([.][0-9]+)?$ ]] || { echo "--sleep wants a number" >&2; exit 2; }
if [[ "$use_tor" -eq 1 ]] && ! command -v torsocks >/dev/null; then
  echo "--tor: torsocks not found on PATH" >&2; exit 2
fi

# CMC has no snapshot for the current (incomplete) day; clamp so a default run
# doesn't always end with one guaranteed failure.
today="$(date -u +%F)"
if [[ "$(date -d "$to" +%s)" -ge "$(date -d "$today" +%s)" ]]; then
  to="$(date -d "$today - 1 day" +%F)"
  echo "note: --to clamped to $to (no CMC snapshot for $today yet)" >&2
fi

from_epoch="$(date -d "$from" +%s)"
to_epoch="$(date -d "$to" +%s)"
(( from_epoch <= to_epoch )) || { echo "--from ($from) is after --to ($to)" >&2; exit 2; }

mkdir -p "$out_dir"

tmp=""
cleanup() { [[ -n "$tmp" && -f "$tmp" ]] && rm -f "$tmp"; return 0; }
trap cleanup EXIT

ua='Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0 Safari/537.36'
api='https://api.coinmarketcap.com/data-api/v3/cryptocurrency/listings/historical'

# One row per coin, ranked by CMC rank; numbers canonicalised (CMC emits some as
# `0E-8`), a missing pct_change left as an empty field, strings CSV-quoted (a
# coin name can carry a comma). `$q` here is a jq binding, not a shell var.
# shellcheck disable=SC2016
row_filter='
  def num(v): if v == null then null else v * 1 end;
  [ .data[]
    | select(.cmcRank != null and (.quotes | length) > 0)
    | .quotes[0] as $q
    | select($q.marketCap != null and $q.price != null)
    | { rank:  .cmcRank,
        id:    .id,
        sym:   .symbol,
        name:  .name,
        mcap:  num($q.marketCap),
        price: num($q.price),
        vol:   num($q.volume24h // 0),
        p1h:   num($q.percentChange1h),
        p24h:  num($q.percentChange24h),
        p7d:   num($q.percentChange7d) } ]
  | sort_by(.rank) | .[]
  | [ .rank, .id, .sym, .name, .mcap, .price, .vol, .p1h, .p24h, .p7d ] | @csv
'

# curl + response code on the last line; routed through torsocks once $use_tor is on.
fetch() {
  local url="$1"
  local runner=(curl)
  [[ "$use_tor" -eq 1 ]] && runner=(torsocks curl)
  "${runner[@]}" -sS -m 60 \
    -H "User-Agent: $ua" -H 'Accept: application/json' \
    -w '\n%{http_code}' "$url" 2>/dev/null || true
}

n_fetched=0
n_skipped=0
n_failed=0

d="$from"
while [[ "$(date -d "$d" +%s)" -le "$to_epoch" ]]; do
  ymd="$(date -d "$d" +%Y%m%d)"
  csv="$out_dir/$ymd.csv"
  next_d="$(date -d "$d + 1 day" +%F)"

  if [[ -f "$csv" && "$refresh" -eq 0 ]]; then
    n_skipped=$((n_skipped + 1))
    d="$next_d"
    continue
  fi

  url="$api?date=$d&start=1&limit=$limit&convert=USD"
  resp="$(fetch "$url")"
  code="${resp##*$'\n'}"
  body="${resp%$'\n'*}"

  # Rate-limited / blocked: flip to Tor once and retry this date.
  if [[ ( "$code" == "429" || "$code" == "403" ) && "$use_tor" -eq 0 ]] \
     && command -v torsocks >/dev/null; then
    echo "! $d: HTTP $code — routing the rest of the run through torsocks" >&2
    use_tor=1
    resp="$(fetch "$url")"
    code="${resp##*$'\n'}"
    body="${resp%$'\n'*}"
  fi

  if [[ "$code" != "200" ]]; then
    echo "! $d: HTTP ${code:-000} — skipped" >&2
    n_failed=$((n_failed + 1))
    d="$next_d"; sleep "$sleep_s"; continue
  fi

  err="$(jq -r '.status.error_code // "0"' <<<"$body" 2>/dev/null || echo parse)"
  if [[ "$err" != "0" ]]; then
    msg="$(jq -r '.status.error_message // "unknown"' <<<"$body" 2>/dev/null || echo "unparseable response")"
    echo "! $d: CMC error $err ($msg) — skipped" >&2
    n_failed=$((n_failed + 1))
    d="$next_d"; sleep "$sleep_s"; continue
  fi

  rows="$(jq -r "$row_filter" <<<"$body" 2>/dev/null || true)"
  if [[ -z "$rows" ]]; then
    echo "! $d: empty / unparseable snapshot — skipped" >&2
    n_failed=$((n_failed + 1))
    d="$next_d"; sleep "$sleep_s"; continue
  fi

  # Write via a temp file in the same dir so the rename is atomic — an
  # interrupted run never leaves a half-written .csv that the next run skips.
  tmp="$(mktemp "$out_dir/.$ymd.XXXXXX")"
  { echo "rank,cmc_id,symbol,name,market_cap_usd,price_usd,volume_24h_usd,pct_change_1h,pct_change_24h,pct_change_7d"
    echo "$rows"
  } > "$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$csv"
  tmp=""
  n_fetched=$((n_fetched + 1))
  n_rows=$(( $(wc -l < "$csv") - 1 ))
  if [[ "$n_rows" -lt "$limit" ]]; then
    echo "  $d -> coins/cmc/$ymd.csv (~ $n_rows/$limit coins — CMC returned a short snapshot)"
  else
    echo "  $d -> coins/cmc/$ymd.csv ($n_rows coins)"
  fi

  d="$next_d"
  sleep "$sleep_s"
done

echo "$n_fetched fetched / $n_skipped skipped / $n_failed failed" >&2
