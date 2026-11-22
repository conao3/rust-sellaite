.PHONY: check
check:
	nix fmt -- --ci
	cargo clippy --all-targets -- -D warnings
	cargo deny --locked check advisories licenses
	lake -d lean build --wfail
	lake -d audit build --wfail
	git diff --exit-code -- .leanfran lean
	test -z "$$(git ls-files --others --exclude-standard -- .leanfran lean)"
	cargo run --quiet -- schema get-sdl --output text | diff -u schema.graphql -
	cargo test

.PHONY: format
format:
	nix fmt
