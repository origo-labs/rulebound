# Checks

Each `[[gates]]` entry selects a check using its `check` field. A gate can report findings, pass, be skipped when its severity is `off`, or fail to produce a trustworthy result. An unavailable or unimplemented check does not pass silently.

## Implemented checks

| Check | What it checks | Notes |
| --- | --- | --- |
| `checks_green` | Runs `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test`. | The most expensive built-in check. It can also provide a test inventory to `tests_not_deleted`. |
| `dependency_surface_unchanged` | Detects changes to declared production dependencies, features, and default-feature settings. | Add exact dependency entries to `allow` when a change is reviewed and intended. |
| `tests_not_deleted` | Detects removed tests, benches, fuzz targets, or tests newly marked `#[ignore]`. | To check that source tests are actually discovered by the compiled test suite, configure it to consume `checks_green`. |
| `paths_unchanged` | Detects edits under configured frozen paths. | Set `paths` on this gate. The generated contract starts with `fixtures` and `testdata`. |
| `public_api_unchanged` | Compares Rust public item signatures against the baseline. | Additions are reported at `warn`; use `allow` for reviewed item paths. It does not prove semantic compatibility. |
| `unsafe_surface_unchanged` | Detects growth in unsafe blocks, functions, impls, and extern blocks. | Existing unsafe code is not itself a finding; the check looks for added unsafe surface. |
| `suppressions_not_widened` | Detects added or broadened diagnostic suppression attributes such as `#[allow]`. | Narrowing or removing a suppression is not a finding. |
| `external_tool` | Runs a configured third-party tool and parses its output. | Configure `tool`, non-empty `args`, and optionally `format`. The external program must be installed in the environment. |

The checks compare the repository's current working tree with the selected baseline where comparison is relevant. `check` expects a diff; an empty observation is an error because it cannot establish that a proposed change was inspected.

## Checks not implemented in this build

These names are part of the contract vocabulary, but declaring one does not make it available:

- `secret_absent` is not implemented. Rulebound reports an error instead of claiming secrets were checked.
- `judged` is reserved for human or model judgement and is not implemented in this version.

The built-in `contract_not_loosened` and `contract_review_stale` checks are implemented and always run. They audit the contract and cannot be configured or switched off. See [Configuration](configuration.md) for how to record justified contract changes and maintain its review date.

## Check limits

Rulebound checks the properties listed in the contract. It does not judge design quality, semantic equivalence, security, test quality, or whether a change is strategically right. An intact observation is also bounded by the configured byte budget; if the budget truncates relevant input, the result is an error rather than a pass. Keep uncovered concerns in `[judgement].not_covered` and have a person review them.
