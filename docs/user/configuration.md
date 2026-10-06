# Configuration

Rulebound reads `rulebound.toml` from the Git repository root. `rulebound init` generates a version 1 contract with a useful starting set of checks. The parser rejects unknown keys, invalid values, duplicate gate IDs, and unsupported contract versions. Correct spelling matters: a typo cannot silently turn off a check.

## Contract fields

The top-level `version` must be `1`. The `[baseline]` table chooses the Git reference used by checks that compare two trees. For example:

```toml
version = 1
baseline = { ref = "main" }

[budget]
observation_bytes = 131072
```

`rulebound check --base <ref>` overrides the configured baseline for one run. The default observation budget is 131072 bytes. Set `[budget].observation_bytes` to change it, or use `rulebound check --budget <bytes>` for a one-run override. The minimum accepted budget is 1024 bytes.

Each `[[gates]]` table declares one check:

```toml
[[gates]]
id = "checks_green"
check = "checks_green"
severity = "warn"
timeout_seconds = 900
```

`id` is a unique identifier containing lowercase letters, digits, `_`, or `-`, and it cannot start with a digit. `check` selects a primitive described in [Checks](checks.md). `severity` is optional and defaults to `warn`.

The severity values are:

- `off`: skip the check. The report shows it was not checked.
- `warn`: report findings without blocking.
- `error`: a finding blocks the change.
- `escalate`: a finding requests human review. If there is no blocking finding, the verdict is `escalate`.

Only promote a gate to `error` after reviewing its behavior on your project and recording the evidence for that choice. A `calibration` string can identify that evidence in the gate declaration.

## Gate-specific fields

- `paths` applies to `paths_unchanged`; list the frozen paths to watch.
- `allow` applies to `dependency_surface_unchanged` and `public_api_unchanged`; it names dependency entries or item paths to permit.
- `timeout_seconds` sets the runtime ceiling for delegated checks. The default is 900 seconds.
- `tool`, `args`, and `format` configure an `external_tool` gate. `format` is `sarif` or `lines`; `args` is a non-empty argument array and can use `{base}`, `{head}`, and `{index}` placeholders.
- `provides` and `consumes` describe relationships between declared gates. The references must name declared gates and cannot form a cycle. For example, `tests_not_deleted` can consume the test inventory from `checks_green`.

Do not set fields that do not apply to the selected check; Rulebound rejects them.

## Judgement and review date

The `[judgement]` table is required:

```toml
[judgement]
reviewed = "2026-10-06"
not_covered = [
  "whether the design is right for the problem",
  "whether tests assert the behavior they claim to assert",
]
```

Use an ISO date (`YYYY-MM-DD`) for `reviewed`. Rulebound reports an error-severity finding when the date is missing, malformed, or more than 180 days old. Update the date when a maintainer has reviewed the contract. Keep `not_covered` as a concrete list of important qualities people still need to judge; Rulebound prints it with each report.

## Recording contract changes

Two built-in checks audit changes to the contract itself. `contract_not_loosened` reports a severity downgrade, removed gate, added allowance or suppression, or removed `not_covered` item unless the same change includes a matching `[[changes]]` record. A recorded reason makes the change visible as a warning; it does not erase the audit trail.

```toml
[[changes]]
gate = "public_api_unchanged"
reason = "Keep API additions non-blocking until the next release cycle is measured."
```

Keep this record in the same change as the contract loosening. Each record must name a declared gate and provide a non-empty reason. `contract_review_stale` and `contract_not_loosened` are always on and cannot be declared or disabled.

## Suppressions

Use a `[[suppressions]]` record to silence a specific finding for a path, with a reason:

```toml
[[suppressions]]
gate = "some_gate_id"
path = "src/legacy.rs"
reason = "Tracked for removal in issue 123."
```

Treat suppressions as reviewed contract changes: the built-in contract audit reports newly added suppressions unless justified in the change record. Prefer a narrow path and a specific reason.
