# Getting started

## Install

Install the CLI from the Git repository:

```sh
cargo install --git https://github.com/origo-labs/rulebound rulebound-cli
```

Check that the binary is available:

```sh
rulebound --help
```

## Create a contract

From the root of the Git repository you want Rulebound to inspect, run:

```sh
rulebound init
```

This writes `rulebound.toml` at the repository root. The generated contract enables the implemented checks at `warn`, so findings are reported but do not block. It also records an initial review date and a starting list of concerns that automated checks do not cover.

If a contract already exists, `init` refuses to replace it. Preview the generated file without writing it with `rulebound init --dry-run`. To intentionally replace the existing contract, pass `--force`.

## Run a check

```sh
rulebound check
```

By default, `check` reads `rulebound.toml` from the repository root and uses its configured baseline, which the generated file sets to `HEAD`. Use `--base <ref>` to compare against another Git commit or reference. `--path <directory>` selects a repository when you are running the command elsewhere.

The first run is useful for seeing what the proposed checks report. Read the output and adjust the contract to match your project before raising any gate to `error`. See [Configuration](configuration.md), [Checks](checks.md), and [Reports and exit codes](reports.md).

## Inspect the observation

`observe` prints the bounded diff and file list Rulebound sees without loading a contract or running gates:

```sh
rulebound observe
```

It uses `HEAD` as the baseline unless you pass `--base <ref>`. To inspect a repository with no changes relative to that baseline, set `--expect-diff=false`. `--budget <bytes>` overrides the default observation limit. A too-small budget is rejected; a truncated observation cannot support a trustworthy result.
