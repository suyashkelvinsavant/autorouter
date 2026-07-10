# Contributing to AutoRouter

Thanks for helping improve AutoRouter.

## Before opening a pull request

1. Open an issue first for substantial changes, so the direction can be agreed before implementation.
2. Keep each pull request focused on one problem.
3. Do not include API keys, local configuration, generated build output, or personally identifying paths.
4. Follow the crate ownership rules in [AGENTS.md](AGENTS.md).

## Development checks

Run these before submitting a pull request:

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cd ui && npm ci && npm run build
```

Add regression coverage for every bug fix. Adapter changes must also cover wire-format streaming shape and completion sentinels.

## Commit and pull-request guidance

- Use an imperative summary, such as `Fix stale session pruning`.
- Explain the user-visible impact and how you tested it.
- Update `README.md`, `manual.md`, or `CHANGELOG.md` when behavior or installation changes.
- By contributing, you agree that your contribution is licensed under the project’s MIT OR Apache-2.0 terms.
