# Contributing to Callimachus

## Development setup

```bash
git clone https://github.com/thehammer/callimachus
cd callimachus
cargo build
cargo test
```

Requires Rust stable (1.85+). No system SQLite needed — the `bundled` feature compiles SQLite in.

## Project structure

See [`README.md`](README.md) for the crate layout. The implementation plan is at
[`docs/plans/callimachus-standalone.md`](docs/plans/callimachus-standalone.md) — read it before
starting any significant work. Each phase has a defined scope; don't expand phase N into phase N+1.

## Code style

- `cargo fmt` before committing (enforced by CI)
- `cargo clippy -- -D warnings` must pass
- Tests live next to the code (`#[cfg(test)]` modules) or in `tests/`
- Behavioral tests only — test what the system does, not how

## Adding an adapter

Adapters implement the `SourceAdapter` trait from `callimachus-core`. See
`docs/plans/callimachus-standalone.md §3` for the contract. Each adapter is a separate crate under
`crates/adapters/`. A new adapter should be under 500 lines of non-test code for the happy path.

## Commits

Conventional commits preferred: `feat:`, `fix:`, `docs:`, `chore:`, `test:`.

## Releasing

Follow these steps to publish a new release:

1. Update the version in all `Cargo.toml` files (`callimachus-core`, `callimachus-cli`, etc.):
   ```bash
   # Find all workspace member Cargo.toml files and update version
   grep -r "^version = " crates/*/Cargo.toml
   ```

2. Update `Cargo.lock`:
   ```bash
   cargo check
   ```

3. Commit the version bump:
   ```bash
   git commit -m "chore: bump version to vX.Y.Z"
   ```

4. Tag and push:
   ```bash
   git tag vX.Y.Z
   git push origin main vX.Y.Z
   ```

5. The GitHub Actions [release workflow](.github/workflows/release.yml) will automatically:
   - Cross-compile binaries for all 5 targets
   - Create a GitHub Release with the binaries attached
   - Generate release notes from commits

6. After the release completes, update the SHA256 values in `Formula/callimachus.rb`:
   ```bash
   # Download each macOS binary and compute SHA256
   curl -L https://github.com/thehammer/callimachus/releases/download/vX.Y.Z/calli-aarch64-apple-darwin | shasum -a 256
   curl -L https://github.com/thehammer/callimachus/releases/download/vX.Y.Z/calli-x86_64-apple-darwin | shasum -a 256
   ```

   Update `Formula/callimachus.rb` with the two SHA256 values, then commit and push:
   ```bash
   git commit -m "chore: update Homebrew formula SHA256 for vX.Y.Z"
   git push origin main
   ```

### Version strings

Use semantic versioning: `MAJOR.MINOR.PATCH`. Pre-release tags (e.g., `v0.2.0-rc1`) will be marked as pre-release on GitHub automatically (any tag containing `-` is treated as pre-release).

## License

By contributing, you agree your contributions are licensed under Apache 2.0.
