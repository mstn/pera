MONTY_REPO := https://github.com/pydantic/monty
MONTY_REV := 60538204fd9cd420d1a93042bb52f7168fb9fca2

.PHONY: dev-env vendor-monty clean-vendor-monty check

dev-env: vendor-monty

# Monty's upstream crates/monty/Cargo.toml relies on Monty's own workspace
# inheritance, so after vendoring we replace it with a standalone manifest
# that Cargo can use inside the Pera workspace.
vendor-monty:
	mkdir -p vendor
	rm -rf vendor/monty
	git clone "$(MONTY_REPO)" vendor/monty
	git -C vendor/monty checkout --force "$(MONTY_REV)"
	rm -rf vendor/monty/.git
	rm -f vendor/monty/crates/monty/build.rs
	cp vendor/monty-crate.Cargo.toml.template vendor/monty/crates/monty/Cargo.toml

clean-vendor-monty:
	rm -rf vendor/monty

check:
	cargo check -p pera-cli
