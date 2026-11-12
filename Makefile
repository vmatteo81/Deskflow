.PHONY: all test deb exe release logo clean

all: test

test:
	cargo test

deb:
	cargo deb

exe:
	cargo build --release --target x86_64-pc-windows-gnu

release:
	./scripts/build-release.sh

logo:
	python3 scripts/render-logo.py

clean:
	cargo clean
	rm -rf dist
