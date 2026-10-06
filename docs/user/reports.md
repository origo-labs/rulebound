# Reports and exit codes

`rulebound check` writes a report to standard output. Choose a format with `--format human`, `--format json`, or `--format sarif`; the default is `human`. Startup and contract-loading errors are written to standard error.

## Verdicts and process status

| Exit code | Verdict | Meaning |
| ---: | --- | --- |
| `0` | `accept` | Every enabled check passed. |
| `1` | `block` | At least one `error`-severity finding was reported. |
| `2` | `error` | Rulebound could not produce a trustworthy verdict, for example because the contract was invalid, a check was unavailable, or the observation was truncated. |
| `3` | `escalate` | At least one `escalate` finding requests human review and no blocking finding took precedence. |

An `error` verdict is distinct from `block`: a check that could not run has not established a rule violation, but it has not passed either. CI should treat both non-zero statuses as requiring attention, while preserving the distinction in reports.

## Human output

The human report lists each gate and its outcome, then the overall verdict, the `not_covered` items, and the contract review date. Findings include the gate ID and a structured summary of the observed change. `off` means the declared gate was skipped; it is not a pass.

## JSON output

JSON keeps three concerns separate at the top level:

- `acceptance`: verdict, exit code, gate outcomes, baseline, and observation budget.
- `rule_violations`: count and structured findings, including severity, subject, expected and observed values, path, and fingerprint.
- `residual_judgement`: whether that capability is implemented, its findings, and the contract's `not_covered` list and review date.

This schema lets consumers handle rule findings and human judgement separately. In this version, residual judgement is not implemented and its findings array is empty.

## SARIF output

SARIF output uses version 2.1.0 and can be uploaded to code-scanning services such as GitHub code scanning. Findings carry stable fingerprints so compatible services can track them across runs. The report properties include the verdict, review date, uncovered items, and names of the built-in contract checks.
