# Embedded check providers

An embedded provider is a Rust implementation registered by the application that calls `rulebound-service`. Rulebound does not load dynamic libraries or discover providers automatically.

## Register a provider

Implement `EmbeddedCheckProvider` in the consumer crate. Register each provider under the same identifier used by its contract gate.

```rust
let mut providers = ProviderRegistry::default();
providers.register("structural_analysis", Arc::new(MyProvider))?;
```

Provider identifiers are case-sensitive. They may contain ASCII letters, digits, `_`, `-`, and `.`. They must contain 1 to 128 bytes. Registration rejects invalid and duplicate identifiers.

An enabled `embedded` gate without a registered provider produces an untrustworthy result. A gate with severity `off` is skipped before provider lookup.

```toml
[[gates]]
id = "structure"
check = "embedded"
provider = "structural_analysis"
policy_path = ".analysis-policy.toml"
severity = "error"
```

`policy_path` is optional and must be a safe repository-relative path. Rulebound reads its full bytes from the candidate snapshot. If the path is declared but absent, evaluation is untrustworthy. The provider declares whether a policy path is required and validates the policy contents itself.

## Evaluate explicit snapshots

Create complete baseline and candidate `Snapshot` values. The candidate snapshot must contain the exact contract bytes passed to `SnapshotEvaluationRequest::builder`. Set `EvaluationOptions::execution_root` to a private materialization whose full snapshot identity matches the candidate. Supply an observation budget and a `ProviderRegistry` when building the request.

The provider receives the gate, both snapshots, optional candidate policy bytes, a cooperative cancellation token, and resource limits. It returns findings, coverage, a policy fingerprint, implementation version, and analyzer identity. Rulebound maps each finding to the outer gate severity and retains the provider's rule ID, fingerprint, source severity, analyzer identity, and related locations.

The `Evaluation` result lists every changed path with baseline and candidate content digests. It includes snapshot modes when the caller supplied snapshots, plus the bounded observation diff and its truncation flag. Include those values in `ReportContext` to render them in human, JSON, and SARIF reports. A declared policy file therefore appears among changed paths with its digests and available diff.

For an enabled gate, coverage is incomplete when the provider marks the result incomplete or omits a required path from its inspected paths. Incomplete coverage makes the gate untrustworthy, regardless of the gate severity. A gate with severity `off` is skipped and does not invoke the provider.

The time limit is checked after provider analysis returns. Providers must honor cancellation cooperatively; Rulebound does not terminate provider threads. Process checks also receive the cancellation token and terminate their direct child process. A provider should not leave background workers running after `analyze` returns.

Rulebound retains at most 2 MiB from each child output stream and continues draining excess output. It appends a truncation marker and sets `outputTruncated` in command evidence. On Unix, Rulebound starts each process in its own process group and terminates that group on timeout, cancellation, or parent exit. Other platforms terminate the direct child. A process that deliberately detaches into another group is outside this cleanup boundary.

## Run the consumer example

The `embedded_provider` example registers a small TODO-marker analyzer. It expects a baseline directory, a candidate directory, and the resolved baseline commit identifier.

Add a complete contract to the candidate directory. Include this gate and the required judgement section:

```toml
version = 1

[[gates]]
id = "todo_markers"
check = "embedded"
provider = "example-todo"
severity = "warn"

[judgement]
reviewed = "2026-10-15"
not_covered = ["design quality"]
```

Run the example from the workspace root:

```sh
cargo run -p rulebound-service --example embedded_provider -- \
  <BASELINE_DIR> <CANDIDATE_DIR> <BASELINE_COMMIT>
```

The candidate directory is also the execution root. Keep its contents unchanged during evaluation. The example prints the reduced verdict and each gate outcome.
