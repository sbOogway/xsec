# End-to-end: run the backtest, then render its tearsheet.
#
#   make tearsheet            # fresh run, generated UUID-7
#   make tearsheet UUID=<id>  # pin / re-render a specific run id
#
# Everything for a run is keyed by $(UUID):
#   logs/<UUID>/logs.log
#   runs/<UUID>/{config,legs,portfolio,fills}.csv
#   runs/<UUID>/tearsheet.html

SHELL := bash
.SHELLFLAGS := -o pipefail -c

# Generate a UUID-7 unless one was passed on the command line, then freeze it
# (:= ) so every recipe in a single `make` invocation sees the same id.
UUID ?= $(shell uuidgen -7 2>/dev/null || uuidgen)
UUID := $(UUID)

.PHONY: tearsheet backtest report

## Run the backtest and build the tearsheet for $(UUID).
tearsheet: backtest report

## Run the backtest binary, tee-ing its output to logs/<UUID>/logs.log.
backtest:
	@mkdir -p logs/$(UUID)
	cargo run --bin xsectional-rs -- --uuid "$(UUID)" 2>&1 | tee logs/$(UUID)/logs.log

## Render runs/<UUID>/tearsheet.html from the captured CSVs.
report:
	uv run --project analysis analysis/tearsheet.py --uuid "$(UUID)"
