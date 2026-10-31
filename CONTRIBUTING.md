# Contributing to InvoiceShield

This is a private repository. Please don't copy code, data or results out of it without asking the owner.

## Setup

- Rust stable (the governor lives in `governor/`, built with `cargo ... --locked`)
- Python 3.12 (`pip install -r requirements.txt -r requirements-dev.txt`)
- Docker, if you want to run the image or compose smoke tests

## Before you open a pull request

Run the same checks CI runs (`.github/workflows/ci.yml`):

```bash
cd governor && cargo fmt --check && cargo clippy --all-targets --locked -- -D warnings && cargo test --locked && cd ..
ruff check .
python -m pytest
python -m eval.demo        # offline replay, no API key, no network
```

`make test` runs the Rust and Python checks together; the other targets are listed at the top of the `Makefile`.

## Branches and pull requests

- `main` is protected: changes land through a pull request, and every required CI job must pass before it can merge.
- Name branches by purpose: `fix/...`, `hardening/...`, `eval/...`, `chore/...`, `docs/...`.
- Keep a pull request to one change, and say in the description what it changes and how you checked it.
- Dependabot opens weekly update PRs. Minor and patch Cargo/pip updates are grouped; Cargo semver-major updates for crates that broke the build are ignored in `.github/dependabot.yml`.

## Evaluation rules

The results in the README come from frozen code and sealed held-out sets. To keep them honest:

- Do not change gate, extractor, checker or governor code in an eval branch. A held-out run uses the code at its freeze tag (for example `freeze-v3`); never move or delete those tags.
- Do not open `labels/` of a sealed set before all verdicts for that run are written.
- Tune only on the dev set, `dataset/clean`, or sets already seen, and say so when you report numbers from them.
- Report results only from files in `eval/results/`, with their paths. Re-runs on seen data are never presented as blind.
- Record decisions and their reasons in `docs/DECISIONS.md`.

## Security

Never commit secrets, API keys, or real business identifiers (real GSTINs, bank accounts, invoices that aren't openly licensed). Report a security problem to the owner privately rather than in an issue. The threat model is in `docs/THREAT_MODEL.md`.

## Licence

By contributing you agree your contribution is released under the repository's MIT licence (`LICENSE`).
