//! Minimal consumer-owned embedded provider over explicit source snapshots.

use std::collections::BTreeSet;
use std::error::Error;
use std::path::PathBuf;
use std::sync::Arc;

use rulebound_contract::parse::CONTRACT_FILENAME;
use rulebound_contract::parse::parse_contract;
use rulebound_observe::Budget;
use rulebound_observe::snapshot::Snapshot;
use rulebound_service::{
    EmbeddedCheckProvider, EmbeddedFinding, EmbeddedProviderError, EmbeddedProviderRequest,
    EmbeddedProviderResult, EvaluationOptions, ProviderCoverage, ProviderRegistry, ResourceLimits,
    SnapshotEvaluationRequest, evaluate_snapshots,
};

struct TodoProvider;

impl EmbeddedCheckProvider for TodoProvider {
    fn configuration_required(&self) -> bool {
        false
    }

    fn analyze(
        &self,
        request: EmbeddedProviderRequest<'_>,
    ) -> Result<EmbeddedProviderResult, EmbeddedProviderError> {
        let mut findings = Vec::new();
        let inspected_paths: BTreeSet<String> = request
            .candidate
            .entries()
            .map(|entry| entry.path().to_string())
            .collect();
        for entry in request.candidate.entries() {
            if request.cancellation.is_cancelled() {
                return Err(EmbeddedProviderError::Cancelled);
            }
            if let Ok(source) = std::str::from_utf8(entry.content()) {
                for (index, line) in source.lines().enumerate() {
                    if line.contains("TODO") {
                        let location = format!("{}:{}", entry.path(), index + 1);
                        let fingerprint = blake3::hash(location.as_bytes()).to_hex().to_string();
                        findings.push(EmbeddedFinding {
                            rule_id: "todo-marker".to_string(),
                            fingerprint,
                            source_severity: "warning".to_string(),
                            message: "TODO marker remains in the candidate".to_string(),
                            path: Some(entry.path().to_string()),
                            line: Some((index + 1) as u32),
                            expected: None,
                            observed: Some(line.to_string()),
                            related_locations: Vec::new(),
                        });
                    }
                }
            }
        }
        let policy = request.policy_bytes.unwrap_or_default();
        Ok(EmbeddedProviderResult {
            findings,
            coverage: ProviderCoverage {
                inspected_paths: inspected_paths.clone(),
                required_paths: inspected_paths,
                complete: true,
            },
            effective_policy_fingerprint: blake3::hash(policy).to_hex().to_string(),
            implementation_version: env!("CARGO_PKG_VERSION").to_string(),
            analyzer_identity: "example-todo-provider".to_string(),
            analyzer_fingerprint: blake3::hash(b"example-todo-provider-v1")
                .to_hex()
                .to_string(),
        })
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1);
    let baseline_root = PathBuf::from(
        args.next()
            .ok_or("usage: embedded_provider BASELINE_DIR CANDIDATE_DIR BASELINE_COMMIT")?,
    );
    let candidate_root = PathBuf::from(args.next().ok_or("missing candidate directory")?);
    let baseline_commit = args
        .next()
        .ok_or("missing baseline commit identifier")?
        .to_string_lossy()
        .into_owned();

    let baseline = Snapshot::from_directory(&baseline_root, &[])?;
    let candidate = Snapshot::from_directory(&candidate_root, &[])?;
    let contract_entry = candidate
        .entry(CONTRACT_FILENAME)
        .ok_or("candidate snapshot does not contain rulebound.toml")?;
    let contract_bytes = contract_entry.content();
    let contract_text = std::str::from_utf8(contract_bytes)?;
    let contract = parse_contract(contract_text)?;

    let mut providers = ProviderRegistry::default();
    providers.register("example-todo", Arc::new(TodoProvider))?;
    let options = EvaluationOptions {
        execution_root: candidate_root,
        environment: rulebound_exec::EnvironmentPolicy::inherit_all(),
        baseline: baseline_commit,
        candidate: candidate.identity().to_string(),
        now_unix: 0,
        cargo_version: "not-used-by-this-contract".to_string(),
        external_tool_version: "not-used-by-this-contract".to_string(),
        cancellation: Default::default(),
        resource_limits: ResourceLimits::default(),
    };
    let request = SnapshotEvaluationRequest::builder(
        &contract,
        contract_bytes,
        &baseline,
        &candidate,
        &options,
        &providers,
    )
    .observation_budget(Budget::DEFAULT)
    .build()?;
    let evaluation = evaluate_snapshots(request)?;
    println!("verdict: {}", evaluation.verdict);
    for run in evaluation.runs {
        println!("{}: {:?}", run.gate_id, run.outcome);
    }
    Ok(())
}
