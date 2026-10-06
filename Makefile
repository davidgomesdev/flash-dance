# `make help` lists everything. Pass args with ARGS, e.g. `make effects ARGS=fire`.
APPS := effects paint palette whack simon snake life

.PHONY: help $(APPS) web meter test check

help:
	@echo "apps:  $(APPS)   (ARGS=<effect> for effects, e.g. make effects ARGS=fire)"
	@echo "other: web  meter (ARGS=<source>)  test  check"

$(APPS):
	cargo run -p $@ -- $(ARGS)

web:
	cargo leptos serve

meter:
	cargo run -p flash-dance-audio --example meter -- $(ARGS)

test:
	cargo test --workspace

check:
	cargo fmt --check && cargo clippy --workspace
