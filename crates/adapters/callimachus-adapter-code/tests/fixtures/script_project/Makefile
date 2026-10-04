.PHONY: build test clean

build: deps
	cargo build --release --locked
	@echo "build finished"
	@echo "artifacts are in target/release"

test: build
	cargo test --workspace --all-features
	@echo "tests finished"
	@echo "see target/debug for test binaries"

clean:
	cargo clean
	rm -rf dist coverage tmp
	@echo "workspace cleaned of build output and coverage reports"
