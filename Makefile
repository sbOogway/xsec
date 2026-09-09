# End-to-end: run the backtest, then render its tearsheet.
#
#   make tearsheet                          # fresh run, generated UUID-7
#   make tearsheet UUID=<id>                # pin / re-render a specific run id
#   make tearsheet ARGS="--slow-days 14"    # pass extra flags to the strategy
#   make tearsheet ARGS="--short-n 0 --regime-filter true"
#
# Everything for a run is keyed by $(UUID):
#   logs/<UUID>/logs.log
#   runs/<UUID>/{config,legs,portfolio,fills}.csv
#   runs/<UUID>/{tearsheet,legs}.html
#
# See `cargo run --bin xsec -- $(STRATEGY) --help` for the strategy's knobs.
#
#   make optimize                                  # Optuna search, 50 trials, defaults
#   make optimize OPT_ARGS="--n-trials 200"         # see analysis/optimize.py --help

SHELL := bash
.SHELLFLAGS := -o pipefail -c

# Generate a UUID-7 unless one was passed on the command line, then freeze it
# (:= ) so every recipe in a single `make` invocation sees the same id.
UUID ?= $(shell uuidgen -7 2>/dev/null || uuidgen)
UUID := $(UUID)

# Which strategy subcommand to run. Every strategy shares the run-level flags
# (--uuid, --universe, --date-*, --starting-balance).
STRATEGY ?= momentum

# The coin universe and the exchange. `make fetch` and `make backtest` both read
# them, so a non-default value stays consistent across the two:
#   make fetch UNIVERSE=coins/my_universe.txt
#   make tearsheet UNIVERSE=coins/my_universe.txt
UNIVERSE ?= universe.txt
EXCHANGE ?= bybit

.PHONY: fetch tearsheet backtest report optimize snapshot_bybit_top snapshot_coingecko_top snapshot_coingecko_bybit_top snapshot_cmc_history universe_cmc_union cmc_resolution

## Download $(EXCHANGE) instruments + bar history for $(UNIVERSE) into
## data/$(EXCHANGE)/. Run once before `make backtest` / `make tearsheet`; re-run
## to refresh (FETCH_ARGS="--refresh" forces a re-download of still-fresh caches).
fetch:
	cargo run --bin xsec -- fetch --exchange "$(EXCHANGE)" --universe "$(UNIVERSE)" $(FETCH_ARGS)

## Run the backtest and build the tearsheet for $(UUID).
tearsheet: backtest report

## Run the backtest binary, tee-ing its output to logs/<UUID>/logs.log.
## Reads the data/$(EXCHANGE)/ cache only — run `make fetch` first.
## Extra flags: make backtest ARGS="--top-n 3 --short-n 3 --long-w 0.7"
backtest:
	@mkdir -p logs/$(UUID)
	cargo run --bin xsec -- --uuid "$(UUID)" --exchange "$(EXCHANGE)" --universe "$(UNIVERSE)" $(STRATEGY) $(ARGS) 2>&1 | tee logs/$(UUID)/logs.log

## Render runs/<UUID>/{tearsheet,legs}.html from the captured CSVs.
report:
	uv run --project analysis analysis/tearsheet.py --uuid "$(UUID)"
	uv run --project analysis analysis/legs.py --uuid "$(UUID)"

## Run an Optuna search over momentum's flags (in-sample), then validate the
## best trial out-of-sample. Extra flags: make optimize OPT_ARGS="--n-trials 200"
optimize:
	uv run --project analysis analysis/optimize.py $(OPT_ARGS)


UNIVERSE_SIZE ?= 100
snapshot_bybit_top:
	./scripts/bybit_turnover_ranking.sh | tac | cut -d':' -f1 | rg 'USDT$$' | head -$(UNIVERSE_SIZE) | sed 's/USDT//' > coins/bybit_top_$(UNIVERSE_SIZE)_$(shell date --utc +%Y-%m-%dT%H:%M:%S%Z).txt

## Snapshot the CoinGecko top $(UNIVERSE_SIZE) into coins/coingecko_top_<N>_<ts>.txt.
## Ranked by market cap; override with CG_BY=volume.
CG_BY ?= mcap
snapshot_coingecko_top:
	./scripts/coingecko_top_ranking.sh --by $(CG_BY) | tac | cut -d':' -f1 | head -$(UNIVERSE_SIZE) > coins/coingecko_top_$(UNIVERSE_SIZE)_$(shell date --utc +%Y-%m-%dT%H:%M:%S%Z).txt

## The CoinGecko top $(UNIVERSE_SIZE) as a ready-to-use universe file at
## coins/coingecko_bybit_top_<N>_<ts>.txt: tradeable names as their Bybit base
## coin, stablecoins and names with no Bybit USDT perp commented out with the
## reason. Fewer than N names are live (the commented ones still count toward N).
snapshot_coingecko_bybit_top:
	ts=$$(date --utc +%Y-%m-%dT%H:%M:%S%Z); { \
	  echo "# CoinGecko top $(UNIVERSE_SIZE) by $(CG_BY) as of $$ts;"; \
	  echo "# stablecoins and names with no Bybit USDT perp are commented out."; \
	  echo; \
	  ./scripts/coingecko_top_ranking.sh --all --by $(CG_BY) | tac | cut -d':' -f1 | head -$(UNIVERSE_SIZE) \
	    | ./scripts/bybit_listing_check.sh --annotate; \
	} > coins/coingecko_bybit_top_$(UNIVERSE_SIZE)_$$ts.txt

# CoinMarketCap historical snapshots: date range and ranking depth. `make
# universe_cmc_union` scrapes any missing days first, so setting these on either
# target keeps the scrape and the flattened universe consistent.
CMC_FROM ?= 2020-01-01
CMC_TO ?= $(shell date --utc +%F)
CMC_LIMIT ?= 200

## Scrape CoinMarketCap's historical daily market-cap rankings into the
## coins/cmc/ local cache (gitignored, like data/), one file per day over
## [CMC_FROM, CMC_TO]:
##   make snapshot_cmc_history CMC_FROM=2024-01-01 CMC_TO=2024-02-01 CMC_LIMIT=200
## Idempotent — re-run to resume; SCRAPE_ARGS="--refresh" re-fetches existing
## days, SCRAPE_ARGS="--tor" routes requests through Tor.
snapshot_cmc_history:
	./scripts/coinmarketcap_snapshot_scrape.sh --from $(CMC_FROM) --to $(CMC_TO) --limit $(CMC_LIMIT) $(SCRAPE_ARGS)

## The union of every base asset that appeared in any coins/cmc/ snapshot in
## [CMC_FROM, CMC_TO] as a ready-to-use universe file at
## coins/cmc_union_<from>_<to>_<ts>.txt: tradeable names as their Bybit base
## coin, stablecoins and names with no Bybit USDT perp commented out with the
## reason. Scrapes any missing days first.
universe_cmc_union: snapshot_cmc_history cmc_resolution
	ts=$$(date --utc +%Y-%m-%dT%H:%M:%S%Z); \
	from=$$(echo "$(CMC_FROM)" | tr -d '-'); to=$$(echo "$(CMC_TO)" | tr -d '-'); \
	{ \
	  echo "# CoinMarketCap historical top $(CMC_LIMIT) union, $(CMC_FROM)..$(CMC_TO) as of $$ts;"; \
	  echo "# every base asset that appeared in any daily snapshot in range;"; \
	  echo "# stablecoins and names with no Bybit USDT perp are commented out."; \
	  echo; \
	  for f in coins/cmc/*.csv; do \
	    d=$$(basename "$$f" .csv); \
	    [[ "$$d" =~ ^[0-9]{8}$$ ]] || continue; \
	    (( 10#$$d >= 10#$$from && 10#$$d <= 10#$$to )) || continue; \
	    tail -n +2 "$$f" | cut -d, -f3 | tr -d '"'; \
	  done | sort -u | ./scripts/bybit_listing_check.sh --annotate; \
	} > coins/cmc_union_$(CMC_FROM)_$(CMC_TO)_$$ts.txt

## Resolve every CMC symbol that has appeared in ANY coins/cmc/ snapshot (whole
## history, not [CMC_FROM, CMC_TO]) to its Bybit base coin, at
## coins/cmc_resolution.csv: "cmc_symbol,bybit_base,status" where status is
## listed|stablecoin|unlisted. Committed; `momentum --source coinmarketcap`
## reads it. Regenerate when Bybit lists new perps.
cmc_resolution: snapshot_cmc_history
	{ \
	  echo "cmc_symbol,bybit_base,status"; \
	  for f in coins/cmc/*.csv; do tail -n +2 "$$f" | cut -d, -f3 | tr -d '"'; done \
	    | sort -u | ./scripts/bybit_listing_check.sh --map; \
	} > coins/cmc_resolution.csv
