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

.PHONY: tearsheet backtest report optimize snapshot_bybit_top snapshot_coingecko_top snapshot_coingecko_bybit_top

## Run the backtest and build the tearsheet for $(UUID).
tearsheet: backtest report

## Run the backtest binary, tee-ing its output to logs/<UUID>/logs.log.
## Extra flags: make backtest ARGS="--top-n 3 --short-n 3 --long-w 0.7"
backtest:
	@mkdir -p logs/$(UUID)
	cargo run --bin xsec -- --uuid "$(UUID)" $(STRATEGY) $(ARGS) 2>&1 | tee logs/$(UUID)/logs.log

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
