---
name: rulebound
description: Set up and use Rulebound to run a repository's declared quality contract. Use when installing Rulebound, initializing or maintaining rulebound.toml, checking a change, interpreting a report, or integrating Rulebound into CI.
---

# Rulebound

Rulebound runs the quality checks declared by a repository in `rulebound.toml`. It does not decide whether code is good or whether a design is right. Treat its verdict as evidence about the declared checks only.

## Set up

1. Install the CLI if it is not already available:

   ```sh
   cargo install --git https://github.com/origo-labs/rulebound rulebound-cli
   ```

2. From the repository root, create a contract:

   ```sh
   rulebound init
   ```

   This writes `rulebound.toml`. It refuses to replace an existing contract. Use `rulebound init --dry-run` to preview a generated contract. Use `rulebound init --force` only when replacement is intended.

3. Review `rulebound.toml` before relying on it. The generated gates start at `warn`, so they report findings without blocking. Check that the baseline, watched paths, and gate settings fit the repository. Keep `[judgement].not_covered` concrete and current; it lists important qualities the automated checks do not assess.

## Check a change

Run from the repository root:

```sh
rulebound check
```

By default, the contract's `[baseline].ref` is used and `HEAD` is the proposed head. Override the baseline for one run with `--base <ref>`. Use `--path <directory>` to select a repository when running elsewhere, `--budget <bytes>` to override the observation limit, or `--format human|json|sarif` to select report output.

For example, to produce SARIF for CI:

```sh
rulebound check --format sarif > results.sarif
```

The selected baseline must exist in the checkout. For pull requests, configure the target branch or merge-base reference deliberately; a missing base, empty expected diff, unavailable gate, invalid contract, or truncated observation means Rulebound could not establish a verdict.

Use `rulebound observe` to inspect the bounded diff and file list Rulebound sees without loading the contract or running checks. It uses `HEAD` as the baseline by default; `--base <ref>` changes it. `--expect-diff=false` permits observing an empty diff. A truncated observation is not trustworthy evidence.

## Interpret results

Exit codes are part of the CLI interface:

| Code | Verdict | Meaning |
| --- | --- | --- |
| 0 | accept | Every enabled check passed. This is not a claim that the change is correct or high quality. |
| 1 | block | At least one `error`-severity finding was reported. |
| 2 | error | Rulebound could not produce a trustworthy verdict; a check that could not run has not passed. |
| 3 | escalate | A finding requests human review and no blocking finding took precedence. |

An `off` gate was skipped; it did not pass. Review the findings and `not_covered` items, especially before treating an `accept` as sufficient for a merge.

## Maintain the contract

- Promote a gate from `warn` to `error` only after reviewing its behavior on this repository and recording the calibration evidence in the gate declaration. A new generated contract has no such calibration.
- Keep the required `[judgement].reviewed` date in ISO `YYYY-MM-DD` format. Rulebound reports an error if it is malformed or more than 180 days old; update it when a maintainer reviews the contract.
- Treat changes that weaken the contract as auditable changes. The always-on `contract_not_loosened` check reports severity downgrades, removed gates, new allowances or suppressions, and removed `not_covered` entries. Add a matching `[[changes]]` entry with a declared gate and a specific reason in the same change.
- `contract_review_stale` and `contract_not_loosened` are always on and cannot be disabled.
- Do not declare `secret_absent` or `judged` expecting them to run: they are not implemented in this version and produce an error. An `external_tool` gate requires the executable to be installed; if it uses a baseline artifact, bind that artifact to the selected base commit and refresh it when the base changes.
- Keep limitations visible. The supported checks do not establish semantic compatibility, security, design quality, or whether tests assert the intended behavior.

## CI

Install Rulebound in the CI job, ensure the checkout contains the selected baseline, and run `rulebound check`. For SARIF, preserve the generated report even when the check exits nonzero, then upload it in a later step that runs after failures. Pin the install to a reviewed tag or commit for repeatable builds. Handle exit code `2` as an inability to assess, distinct from a rule violation at exit code `1`.

For exact contract fields, supported checks, and report schemas, consult the repository's `docs/user/` guide, especially `configuration.md`, `checks.md`, `reports.md`, and `ci.md`.
