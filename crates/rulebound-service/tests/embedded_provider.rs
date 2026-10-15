//! Acceptance checks for consumer-registered embedded providers.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rulebound_contract::parse::parse_contract;
use rulebound_observe::Budget;
use rulebound_observe::snapshot::{Snapshot, SnapshotEntry};
use rulebound_orchestrate::{GateOutcome, UntrustworthyReason, Verdict};
use rulebound_service::{
    EmbeddedCheckProvider, EmbeddedFinding, EmbeddedProviderError, EmbeddedProviderRequest,
    EmbeddedProviderResult, EvaluationOptions, ProviderCoverage, ProviderRegistry, ResourceLimits,
    SnapshotEvaluationRequest, evaluate_snapshots,
};

const CONTRACT: &str = r#"
version = 1

[[gates]]
id = "structure"
check = "embedded"
provider = "test-provider"
severity = "warn"

[judgement]
reviewed = "2026-10-15"
not_covered = ["design quality"]
"#;

struct TestProvider {
    complete: bool,
}

impl EmbeddedCheckProvider for TestProvider {
    fn configuration_required(&self) -> bool {
        false
    }

    fn analyze(
        &self,
        request: EmbeddedProviderRequest<'_>,
    ) -> Result<EmbeddedProviderResult, EmbeddedProviderError> {
        let paths: BTreeSet<String> = request
            .candidate
            .entries()
            .map(|entry| entry.path().to_string())
            .collect();
        Ok(EmbeddedProviderResult {
            findings: vec![EmbeddedFinding {
                rule_id: "sample-rule".to_string(),
                fingerprint: "source-fingerprint".to_string(),
                source_severity: "warning".to_string(),
                message: "sample finding".to_string(),
                path: Some("src/lib.rs".to_string()),
                line: Some(1),
                expected: None,
                observed: Some("candidate".to_string()),
                related_locations: Vec::new(),
            }],
            coverage: ProviderCoverage {
                inspected_paths: paths.clone(),
                required_paths: paths,
                complete: self.complete,
            },
            effective_policy_fingerprint: blake3::hash(b"").to_hex().to_string(),
            implementation_version: "test-1".to_string(),
            analyzer_identity: "test-analyzer".to_string(),
            analyzer_fingerprint: "analyzer-fingerprint".to_string(),
        })
    }
}

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "rulebound-provider-{}-{unique}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        ));
        std::fs::create_dir_all(path.join("src")).expect("create fixture directory");
        std::fs::write(path.join("rulebound.toml"), CONTRACT).expect("write contract");
        std::fs::write(path.join("src/lib.rs"), "candidate\n").expect("write source");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn baseline_snapshot() -> Snapshot {
    Snapshot::new([
        SnapshotEntry::new("rulebound.toml", 0o100644, CONTRACT.as_bytes().to_vec())
            .expect("valid contract snapshot entry"),
        SnapshotEntry::new("src/lib.rs", 0o100644, b"baseline\n".to_vec())
            .expect("valid source snapshot entry"),
    ])
    .expect("valid baseline snapshot")
}

fn evaluate(
    fixture: &Fixture,
    register_provider: bool,
    complete: bool,
) -> rulebound_service::Evaluation {
    let candidate = Snapshot::from_directory(fixture.path(), &[]).expect("read candidate");
    let baseline = baseline_snapshot();
    let contract = parse_contract(CONTRACT).expect("parse contract");
    let mut providers = ProviderRegistry::default();
    if register_provider {
        providers
            .register("test-provider", Arc::new(TestProvider { complete }))
            .expect("register provider");
    }
    let options = EvaluationOptions {
        execution_root: fixture.path().to_path_buf(),
        environment: rulebound_exec::EnvironmentPolicy::allowlist(["PATH"]),
        baseline: "baseline-commit".to_string(),
        candidate: "candidate-snapshot".to_string(),
        now_unix: 0,
        cargo_version: "unused".to_string(),
        external_tool_version: "unused".to_string(),
        cancellation: Default::default(),
        resource_limits: ResourceLimits::default(),
    };
    let request = SnapshotEvaluationRequest::builder(
        &contract,
        CONTRACT.as_bytes(),
        &baseline,
        &candidate,
        &options,
        &providers,
    )
    .observation_budget(Budget::DEFAULT)
    .build()
    .expect("build evaluation request");
    evaluate_snapshots(request).expect("evaluate snapshots")
}

#[test]
fn registered_provider_findings_keep_source_provenance() {
    let fixture = Fixture::new();
    let evaluation = evaluate(&fixture, true, true);
    assert_eq!(evaluation.verdict, Verdict::Accept);
    let run = evaluation
        .runs
        .iter()
        .find(|run| run.gate_id.as_str() == "structure")
        .expect("embedded gate run");
    assert_eq!(run.findings.len(), 1);
    assert_eq!(
        run.findings[0]
            .embedded_metadata
            .as_ref()
            .expect("embedded metadata")
            .source_fingerprint,
        "source-fingerprint"
    );
    assert!(run.coverage.as_ref().expect("coverage").complete);
    let changed_source = evaluation
        .changed_files
        .iter()
        .find(|file| file.path == "src/lib.rs")
        .expect("changed source evidence");
    assert!(changed_source.baseline_digest.is_some());
    assert!(changed_source.candidate_digest.is_some());
    assert_eq!(changed_source.baseline_mode, Some(0o100644));
    assert_eq!(changed_source.candidate_mode, Some(0o100644));
}

#[test]
fn enabled_gate_without_registered_provider_is_untrustworthy() {
    let fixture = Fixture::new();
    let evaluation = evaluate(&fixture, false, true);
    assert_eq!(evaluation.verdict, Verdict::Error);
    let run = evaluation
        .runs
        .iter()
        .find(|run| run.gate_id.as_str() == "structure")
        .expect("embedded gate run");
    assert!(matches!(
        run.outcome,
        GateOutcome::Untrustworthy {
            reason: UntrustworthyReason::ProviderUnavailable { .. },
            ..
        }
    ));
}

#[test]
fn incomplete_provider_coverage_forces_error_and_remains_reported() {
    let fixture = Fixture::new();
    let evaluation = evaluate(&fixture, true, false);
    assert_eq!(evaluation.verdict, Verdict::Error);
    let run = evaluation
        .runs
        .iter()
        .find(|run| run.gate_id.as_str() == "structure")
        .expect("embedded gate run");
    assert!(matches!(run.outcome, GateOutcome::Untrustworthy { .. }));
    assert!(!run.coverage.as_ref().expect("coverage").complete);
    assert_eq!(run.findings.len(), 1);
}
