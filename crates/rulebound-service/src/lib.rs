//! Shared evaluation orchestration for the Rulebound CLI and embedded callers.
//!
//! This crate owns shared evaluation orchestration. The CLI adapts its current
//! Git-backed observation flow, while embedded callers can provide explicit
//! snapshots through the validated snapshot request API.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Duration;
use std::{error::Error, fmt};

use rulebound_contract::{Contract, Gate, GateId, Primitive, Severity};
use rulebound_gates::{GateContext, GateResult, registry};
use rulebound_observe::snapshot::{Snapshot, SnapshotError, SnapshotIdentity};
use rulebound_observe::{Budget, Observation, SnapshotObservationError};
use rulebound_orchestrate::{GateRun, ReductionInput, Verdict, reduce};

pub use rulebound_exec::CancellationToken;

/// Resource bounds passed to each embedded provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceLimits {
    /// Maximum total baseline, candidate, and policy bytes supplied to one
    /// provider.
    pub max_provider_input_bytes: usize,
    /// Maximum findings a provider may return for one gate.
    pub max_findings: usize,
    /// Cooperative time budget for provider analysis.
    pub max_elapsed: Duration,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_provider_input_bytes: 64 * 1024 * 1024,
            max_findings: 10_000,
            max_elapsed: Duration::from_secs(900),
        }
    }
}

/// Inputs visible to an embedded provider for one declared gate.
#[derive(Debug)]
pub struct EmbeddedProviderRequest<'a> {
    /// Gate declaration being evaluated.
    pub gate: &'a Gate,
    /// Complete baseline snapshot.
    pub baseline: &'a Snapshot,
    /// Complete candidate snapshot.
    pub candidate: &'a Snapshot,
    /// Full candidate policy bytes, if `policy_path` was declared.
    pub policy_bytes: Option<&'a [u8]>,
    /// Cooperative cancellation signal.
    pub cancellation: &'a CancellationToken,
    /// Resource bounds for this invocation.
    pub limits: &'a ResourceLimits,
}

/// Provider finding before Rulebound assigns gate identity and consequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedFinding {
    /// Original analyzer rule identifier.
    pub rule_id: String,
    /// Original stable finding fingerprint.
    pub fingerprint: String,
    /// Original source severity, preserved as provided.
    pub source_severity: String,
    /// Human readable description.
    pub message: String,
    /// Primary repository-relative location.
    pub path: Option<String>,
    /// Primary 1-based source line.
    pub line: Option<u32>,
    /// Optional expected value.
    pub expected: Option<String>,
    /// Optional observed value.
    pub observed: Option<String>,
    /// Additional source locations related to the finding.
    pub related_locations: Vec<rulebound_orchestrate::RelatedLocation>,
}

/// Coverage and result completeness returned by an embedded provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCoverage {
    /// Snapshot paths the provider inspected.
    pub inspected_paths: BTreeSet<String>,
    /// Paths that the provider declares necessary for a complete result.
    pub required_paths: BTreeSet<String>,
    /// Whether provider-internal analysis completed successfully.
    pub complete: bool,
}

/// Typed findings, coverage, and identity returned by an embedded provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedProviderResult {
    /// Every finding, without collapsing by severity.
    pub findings: Vec<EmbeddedFinding>,
    /// Coverage and completeness evidence.
    pub coverage: ProviderCoverage,
    /// Effective policy fingerprint used for this run.
    pub effective_policy_fingerprint: String,
    /// Provider implementation version.
    pub implementation_version: String,
    /// Analyzer identity, such as a model or engine name.
    pub analyzer_identity: String,
    /// Fingerprint of the analyzer implementation and configuration.
    pub analyzer_fingerprint: String,
}

/// Errors an embedded provider can return without panicking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbeddedProviderError {
    /// The provider requires a policy file but none was declared.
    ConfigurationRequired,
    /// The supplied policy bytes were invalid for this provider.
    InvalidPolicy(String),
    /// A baseline index or other supplied artifact is incompatible.
    IncompatibleArtifact(String),
    /// The provider observed cooperative cancellation.
    Cancelled,
    /// Analysis could not produce a trustworthy result.
    Analysis(String),
}

impl fmt::Display for EmbeddedProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConfigurationRequired => f.write_str("provider policy is required"),
            Self::InvalidPolicy(detail) => write!(f, "invalid provider policy: {detail}"),
            Self::IncompatibleArtifact(detail) => {
                write!(f, "incompatible provider artifact: {detail}")
            }
            Self::Cancelled => f.write_str("provider analysis was cancelled"),
            Self::Analysis(detail) => f.write_str(detail),
        }
    }
}

impl Error for EmbeddedProviderError {}

/// Public interface implemented by consumer-crate embedded analyzers.
pub trait EmbeddedCheckProvider: Send + Sync {
    /// Whether this provider requires the contract to declare `policy_path`.
    fn configuration_required(&self) -> bool;

    /// Analyze one gate using the supplied snapshots and policy bytes.
    ///
    /// Implementations must honor cancellation cooperatively and must not
    /// spawn an unbounded worker that outlives this call.
    fn analyze(
        &self,
        request: EmbeddedProviderRequest<'_>,
    ) -> Result<EmbeddedProviderResult, EmbeddedProviderError>;
}

/// Explicit set of embedded providers available to one evaluation.
#[derive(Clone, Default)]
pub struct ProviderRegistry {
    providers: BTreeMap<String, Arc<dyn EmbeddedCheckProvider>>,
}

impl fmt::Debug for ProviderRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderRegistry")
            .field("provider_ids", &self.providers.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// Why a provider could not be registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderRegistrationError {
    /// The identifier is not a valid provider key.
    InvalidIdentifier(String),
    /// A provider with the same identifier is already registered.
    DuplicateIdentifier(String),
}

impl fmt::Display for ProviderRegistrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidIdentifier(id) => write!(f, "invalid provider identifier {id:?}"),
            Self::DuplicateIdentifier(id) => write!(f, "provider {id:?} is already registered"),
        }
    }
}

impl Error for ProviderRegistrationError {}

impl ProviderRegistry {
    /// Register one provider under its contract identifier.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid or duplicate identifiers.
    pub fn register(
        &mut self,
        id: impl Into<String>,
        provider: Arc<dyn EmbeddedCheckProvider>,
    ) -> Result<(), ProviderRegistrationError> {
        let id = id.into();
        if !valid_provider_id(&id) {
            return Err(ProviderRegistrationError::InvalidIdentifier(id));
        }
        if self.providers.contains_key(&id) {
            return Err(ProviderRegistrationError::DuplicateIdentifier(id));
        }
        self.providers.insert(id, provider);
        Ok(())
    }

    fn get(&self, id: &str) -> Option<&Arc<dyn EmbeddedCheckProvider>> {
        self.providers.get(id)
    }
}

fn valid_provider_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

/// Inputs that the caller supplies to make an evaluation reproducible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluationOptions {
    /// Root directory in which all process checks run.
    pub execution_root: PathBuf,
    /// Explicit policy for child process environment variables.
    pub environment: rulebound_exec::EnvironmentPolicy,
    /// Resolved baseline commit identifier.
    pub baseline: String,
    /// Candidate reference used to expand external-tool arguments.
    pub candidate: String,
    /// Unix timestamp supplied to time-dependent analyzed checks.
    pub now_unix: i64,
    /// Cargo version recorded as delegated-check provenance.
    pub cargo_version: String,
    /// External tool version recorded as delegated-check provenance.
    pub external_tool_version: String,
    /// Cooperative cancellation shared with embedded providers.
    pub cancellation: CancellationToken,
    /// Resource bounds applied to embedded providers.
    pub resource_limits: ResourceLimits,
}

/// Typed results from evaluating one contract against one observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evaluation {
    /// Every declared gate run, in contract declaration order, followed by
    /// Rulebound's always-on contract checks.
    pub runs: Vec<GateRun>,
    /// Verdict reduced from all returned gate outcomes.
    pub verdict: Verdict,
    /// Identity of the complete baseline snapshot, when explicitly supplied.
    pub baseline_identity: Option<SnapshotIdentity>,
    /// Identity of the complete candidate snapshot, when explicitly supplied.
    pub candidate_identity: Option<SnapshotIdentity>,
    /// Fingerprint of the complete observation coverage and clipping state.
    pub observation_fingerprint: Option<String>,
    /// Fingerprint of the original contract bytes, when provided.
    pub contract_fingerprint: Option<String>,
    /// Trust failures detected while evaluating supplied snapshots.
    pub trust_failures: Vec<EvaluationTrustFailure>,
    /// Allowed environment variable names observed for this evaluation.
    pub allowed_environment_names: Vec<String>,
}

/// A trust failure that invalidates an otherwise completed evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvaluationTrustFailure {
    /// A check changed a declared candidate input in the execution root.
    CandidateMutated {
        /// Identity supplied by the caller.
        expected: SnapshotIdentity,
        /// Identity observed after checks completed.
        actual: SnapshotIdentity,
    },
    /// The execution root could not be read after checks completed.
    ExecutionRootUnavailable(SnapshotError),
}

/// Typed failure while preparing a caller supplied snapshot evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvaluationError {
    /// Original contract bytes could not be parsed as UTF-8 TOML.
    InvalidContractBytes(String),
    /// The parsed original contract bytes differ from the supplied validated
    /// contract value.
    ContractValueMismatch,
    /// Candidate snapshot does not contain the exact supplied contract bytes.
    CandidateContractMismatch,
    /// An explicit observation budget was not supplied.
    MissingObservationBudget,
    /// Snapshot materialization could not be read.
    Snapshot(SnapshotError),
    /// Snapshot content could not be adapted to the current text gates.
    Observation(SnapshotObservationError),
    /// The execution root has a different full identity than the candidate.
    ExecutionRootMismatch {
        /// Identity declared by the candidate snapshot.
        expected: SnapshotIdentity,
        /// Identity read from the execution root.
        actual: SnapshotIdentity,
    },
}

impl fmt::Display for EvaluationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidContractBytes(message) => {
                write!(f, "candidate contract bytes are invalid: {message}")
            }
            Self::ContractValueMismatch => {
                f.write_str("validated contract does not match its original bytes")
            }
            Self::CandidateContractMismatch => {
                f.write_str("candidate snapshot contract bytes do not match supplied bytes")
            }
            Self::MissingObservationBudget => {
                f.write_str("an explicit observation budget is required")
            }
            Self::Snapshot(error) => error.fmt(f),
            Self::Observation(error) => error.fmt(f),
            Self::ExecutionRootMismatch { expected, actual } => write!(
                f,
                "execution root identity {actual} does not match candidate identity {expected}"
            ),
        }
    }
}

impl Error for EvaluationError {}

/// Inputs required to evaluate explicit snapshots against a materialized root.
#[derive(Debug)]
pub struct SnapshotEvaluationRequest<'a> {
    contract: &'a Contract,
    contract_bytes: &'a [u8],
    baseline: &'a Snapshot,
    candidate: &'a Snapshot,
    options: &'a EvaluationOptions,
    observation_budget: Budget,
    expect_change: bool,
    ignored_execution_paths: Vec<String>,
    providers: &'a ProviderRegistry,
}

impl<'a> SnapshotEvaluationRequest<'a> {
    /// Start building an explicit snapshot evaluation request.
    pub fn builder(
        contract: &'a Contract,
        contract_bytes: &'a [u8],
        baseline: &'a Snapshot,
        candidate: &'a Snapshot,
        options: &'a EvaluationOptions,
        providers: &'a ProviderRegistry,
    ) -> SnapshotEvaluationRequestBuilder<'a> {
        SnapshotEvaluationRequestBuilder {
            contract,
            contract_bytes,
            baseline,
            candidate,
            options,
            providers,
            observation_budget: None,
            expect_change: false,
            ignored_execution_paths: Vec::new(),
        }
    }
}

/// Builder for a complete explicit snapshot evaluation request.
#[derive(Debug)]
pub struct SnapshotEvaluationRequestBuilder<'a> {
    contract: &'a Contract,
    contract_bytes: &'a [u8],
    baseline: &'a Snapshot,
    candidate: &'a Snapshot,
    options: &'a EvaluationOptions,
    providers: &'a ProviderRegistry,
    observation_budget: Option<Budget>,
    expect_change: bool,
    ignored_execution_paths: Vec<String>,
}

impl<'a> SnapshotEvaluationRequestBuilder<'a> {
    /// Set the required explicit observation budget.
    pub fn observation_budget(mut self, budget: Budget) -> Self {
        self.observation_budget = Some(budget);
        self
    }

    /// Require at least one candidate change.
    pub fn expect_change(mut self, expect_change: bool) -> Self {
        self.expect_change = expect_change;
        self
    }

    /// Declare ignored output paths within the execution root.
    pub fn ignored_execution_paths(mut self, paths: impl IntoIterator<Item = String>) -> Self {
        self.ignored_execution_paths = paths.into_iter().collect();
        self
    }

    /// Finish the request after all required inputs have been supplied.
    ///
    /// # Errors
    ///
    /// Returns an error when no observation budget was supplied.
    pub fn build(self) -> Result<SnapshotEvaluationRequest<'a>, EvaluationError> {
        let observation_budget = self
            .observation_budget
            .ok_or(EvaluationError::MissingObservationBudget)?;
        Ok(SnapshotEvaluationRequest {
            contract: self.contract,
            contract_bytes: self.contract_bytes,
            baseline: self.baseline,
            candidate: self.candidate,
            options: self.options,
            providers: self.providers,
            observation_budget,
            expect_change: self.expect_change,
            ignored_execution_paths: self.ignored_execution_paths,
        })
    }
}

/// Evaluate a validated contract against a previously captured observation.
///
/// This function performs no repository capture and reads no clock. The
/// caller owns those inputs and must provide versions and references matching
/// the same captured state.
///
/// # Arguments
///
/// * `contract` - Validated contract to evaluate.
/// * `observation` - Captured two-tree observation shared by analyzed gates.
/// * `options` - Resolved baseline, candidate, timestamp, and tool versions.
///
/// # Returns
///
/// All declared gate runs and the reduced verdict.
pub fn evaluate(
    contract: &Contract,
    observation: &Observation,
    options: &EvaluationOptions,
) -> Evaluation {
    evaluate_with_providers(
        contract,
        observation,
        options,
        None,
        None,
        &ProviderRegistry::default(),
    )
}

/// Evaluate a contract with optional complete snapshots and registered embedded providers.
pub fn evaluate_with_providers(
    contract: &Contract,
    observation: &Observation,
    options: &EvaluationOptions,
    baseline: Option<&Snapshot>,
    candidate: Option<&Snapshot>,
    providers: &ProviderRegistry,
) -> Evaluation {
    let mut inventory: Option<rulebound_exec::test_inventory::TestInventory> = None;
    let mut runs = Vec::new();

    for gate in &contract.gates {
        if gate.primitive == Primitive::ChecksGreen
            && contract.gates.iter().any(|consumer| {
                consumer
                    .consumes
                    .iter()
                    .any(|id| id == &gate.id.to_string())
            })
        {
            let (run, provided_inventory) =
                rulebound_exec::checks_green::run_checks_in_with_environment_and_cancellation(
                    rulebound_exec::origin("cargo", &options.cargo_version),
                    timeout(gate),
                    &options.execution_root,
                    &options.environment,
                    &options.cancellation,
                );
            runs.push(run);
            inventory = Some(provided_inventory);
        }
    }

    for gate in &contract.gates {
        if gate.primitive == Primitive::ChecksGreen && inventory.is_some() {
            continue;
        }
        runs.push(run_gate(
            gate,
            observation,
            options,
            inventory.as_ref(),
            baseline,
            candidate,
            providers,
        ));
    }

    runs.extend(built_in_gates(observation, options));
    let mut ordered = Vec::with_capacity(runs.len());
    for gate in &contract.gates {
        if let Some(position) = runs.iter().position(|run| run.gate_id == gate.id) {
            ordered.push(runs.remove(position));
        }
    }
    ordered.extend(runs);

    let outcomes: Vec<_> = ordered.iter().map(|run| run.outcome.clone()).collect();
    let verdict = reduce(&ReductionInput {
        outcomes: &outcomes,
        ..Default::default()
    });

    Evaluation {
        runs: ordered,
        verdict,
        baseline_identity: observation.baseline_identity.clone(),
        candidate_identity: observation.candidate_identity.clone(),
        observation_fingerprint: observation_fingerprint(observation),
        contract_fingerprint: None,
        trust_failures: Vec::new(),
        allowed_environment_names: options.environment.allowed_names(),
    }
}

/// Evaluate supplied snapshots after verifying the candidate contract and
/// materialized execution root.
///
/// The execution root is read before any process check runs. A matching full
/// identity is required, and caller-declared ignored output paths are omitted
/// from both sides of that comparison.
///
/// # Errors
///
/// Returns [`EvaluationError`] for invalid contract bytes, contract mismatch,
/// snapshot read failures, incomplete text adaptation, or root mismatch.
pub fn evaluate_snapshots(
    request: SnapshotEvaluationRequest<'_>,
) -> Result<Evaluation, EvaluationError> {
    let contract_text = std::str::from_utf8(request.contract_bytes)
        .map_err(|error| EvaluationError::InvalidContractBytes(error.to_string()))?;
    let parsed = rulebound_contract::parse::parse_contract(contract_text)
        .map_err(|error| EvaluationError::InvalidContractBytes(error.to_string()))?;
    if parsed != *request.contract {
        return Err(EvaluationError::ContractValueMismatch);
    }
    let contract_path = rulebound_contract::parse::CONTRACT_FILENAME;
    if request
        .candidate
        .entry(contract_path)
        .is_none_or(|entry| entry.content() != request.contract_bytes)
    {
        return Err(EvaluationError::CandidateContractMismatch);
    }

    let actual = Snapshot::from_directory(
        &request.options.execution_root,
        &request.ignored_execution_paths,
    )
    .map_err(EvaluationError::Snapshot)?;
    if actual.identity() != request.candidate.identity() {
        return Err(EvaluationError::ExecutionRootMismatch {
            expected: request.candidate.identity().clone(),
            actual: actual.identity().clone(),
        });
    }

    let observation = Observation::from_snapshots(
        request.baseline,
        request.candidate,
        Some(request.options.baseline.clone()),
        request.observation_budget,
        request.expect_change,
    )
    .map_err(EvaluationError::Observation)?;
    let mut evaluation = evaluate_with_providers(
        request.contract,
        &observation,
        request.options,
        Some(request.baseline),
        Some(request.candidate),
        request.providers,
    );
    evaluation.contract_fingerprint =
        Some(blake3::hash(request.contract_bytes).to_hex().to_string());
    match Snapshot::from_directory(
        &request.options.execution_root,
        &request.ignored_execution_paths,
    ) {
        Ok(after) if after.identity() != request.candidate.identity() => {
            evaluation
                .trust_failures
                .push(EvaluationTrustFailure::CandidateMutated {
                    expected: request.candidate.identity().clone(),
                    actual: after.identity().clone(),
                });
            evaluation.verdict = Verdict::Error;
        }
        Ok(_) => {}
        Err(error) => {
            evaluation
                .trust_failures
                .push(EvaluationTrustFailure::ExecutionRootUnavailable(error));
            evaluation.verdict = Verdict::Error;
        }
    }
    Ok(evaluation)
}

fn observation_fingerprint(observation: &Observation) -> Option<String> {
    let baseline = observation.baseline_identity.as_ref()?;
    let candidate = observation.candidate_identity.as_ref()?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"rulebound-observation-v1\0");
    hasher.update(baseline.as_str().as_bytes());
    hasher.update(candidate.as_str().as_bytes());
    hasher.update(&(observation.budget.bytes() as u64).to_be_bytes());
    hasher.update(&[u8::from(observation.diff.truncated)]);
    if let Some(empty) = observation.empty {
        hasher.update(empty.as_str().as_bytes());
    }
    for file in &observation.files {
        hasher.update(&(file.path.len() as u64).to_be_bytes());
        hasher.update(file.path.as_bytes());
        hasher.update(&[u8::from(file.truncated)]);
    }
    Some(hasher.finalize().to_hex().to_string())
}

fn built_in_gates(observation: &Observation, options: &EvaluationOptions) -> Vec<GateRun> {
    [
        Primitive::ContractNotLoosened,
        Primitive::ContractReviewStale,
    ]
    .into_iter()
    .map(|primitive| {
        let mut gate = Gate::new(
            GateId::new(primitive.as_str()).expect("primitive name is a valid id"),
            primitive,
        );
        gate.severity = Severity::Error;
        run_gate(
            &gate,
            observation,
            options,
            None,
            None,
            None,
            &ProviderRegistry::default(),
        )
    })
    .collect()
}

fn run_gate(
    gate: &Gate,
    observation: &Observation,
    options: &EvaluationOptions,
    inventory: Option<&rulebound_exec::test_inventory::TestInventory>,
    baseline: Option<&Snapshot>,
    candidate: Option<&Snapshot>,
    providers: &ProviderRegistry,
) -> GateRun {
    if gate.severity == Severity::Off {
        return GateRun::skipped(gate.id.clone(), gate.primitive);
    }

    match gate.primitive {
        Primitive::ChecksGreen => {
            rulebound_exec::checks_green::run_checks_in_with_environment_and_cancellation(
                rulebound_exec::origin("cargo", &options.cargo_version),
                timeout(gate),
                &options.execution_root,
                &options.environment,
                &options.cancellation,
            )
            .0
        }
        Primitive::ExternalTool => {
            let Some(tool) = gate.tool.as_deref() else {
                return GateRun::untrustworthy(
                    gate.id.clone(),
                    gate.primitive,
                    rulebound_orchestrate::UntrustworthyReason::ContractInvalid {
                        detail: "an `external_tool` gate declares no `tool`".to_string(),
                    },
                    rulebound_orchestrate::Origin::Delegated {
                        tool: rulebound_orchestrate::ToolId::new("external"),
                        version: String::new(),
                    },
                );
            };
            let format = match gate.format.as_deref() {
                Some("lines") => rulebound_exec::external_tool::OutputFormat::Lines,
                _ => rulebound_exec::external_tool::OutputFormat::Sarif,
            };
            let values = rulebound_exec::external_tool::TemplateValues {
                base: options.baseline.clone(),
                head: options.candidate.clone(),
                index: gate
                    .args
                    .iter()
                    .find_map(|arg| arg.strip_prefix("--index=").map(str::to_string))
                    .unwrap_or_default(),
            };
            rulebound_exec::external_tool::run_tool_in_with_environment_and_cancellation(
                &gate.id,
                tool,
                &gate.args,
                format,
                &values,
                timeout(gate),
                rulebound_exec::origin(tool, &options.external_tool_version),
                &options.execution_root,
                &options.environment,
                &options.cancellation,
            )
        }
        Primitive::Embedded => {
            let provider = gate.provider.as_deref().unwrap_or("<missing>");
            run_embedded(gate, provider, options, baseline, candidate, providers)
        }
        primitive => {
            let result = registry::dispatch(
                primitive,
                &GateContext {
                    gate,
                    observation,
                    test_inventory: inventory,
                    now_unix: options.now_unix,
                },
            );
            let origin = rulebound_orchestrate::Origin::Analyzed { primitive };
            match result {
                GateResult::Clean => GateRun::passed(gate.id.clone(), primitive, origin),
                GateResult::Findings(findings) => {
                    GateRun::with_findings(gate.id.clone(), primitive, findings, origin)
                }
                GateResult::Untrustworthy(reason) => {
                    GateRun::untrustworthy(gate.id.clone(), primitive, reason, origin)
                }
            }
        }
    }
}

fn run_embedded(
    gate: &Gate,
    provider_id: &str,
    options: &EvaluationOptions,
    baseline: Option<&Snapshot>,
    candidate: Option<&Snapshot>,
    providers: &ProviderRegistry,
) -> GateRun {
    use rulebound_orchestrate::{
        EmbeddedFindingMetadata, GateRunBuilder, HunkRef, Origin, RelatedLocation, Side, Subject,
        SubjectKind, UntrustworthyReason,
    };

    let fallback_origin = || Origin::Embedded {
        provider: provider_id.to_string(),
        version: String::new(),
        analyzer_fingerprint: String::new(),
    };
    let untrustworthy =
        |reason| GateRun::untrustworthy(gate.id.clone(), gate.primitive, reason, fallback_origin());
    let Some(provider) = providers.get(provider_id) else {
        return untrustworthy(UntrustworthyReason::ProviderUnavailable {
            provider: provider_id.to_string(),
        });
    };
    let (Some(baseline), Some(candidate)) = (baseline, candidate) else {
        return untrustworthy(UntrustworthyReason::Indeterminate {
            detail: "embedded providers require complete snapshots".to_string(),
        });
    };
    let policy_bytes = match gate.policy_path.as_deref() {
        Some(path) => match candidate.entry(path) {
            Some(entry) => Some(entry.content()),
            None => {
                return untrustworthy(UntrustworthyReason::Indeterminate {
                    detail: format!("provider policy file {path:?} is absent from candidate"),
                });
            }
        },
        None if provider.configuration_required() => {
            return untrustworthy(UntrustworthyReason::Indeterminate {
                detail: "provider requires a declared policy_path".to_string(),
            });
        }
        None => None,
    };
    let input_bytes = baseline
        .total_bytes()
        .saturating_add(candidate.total_bytes())
        .saturating_add(policy_bytes.map_or(0, <[u8]>::len));
    if input_bytes > options.resource_limits.max_provider_input_bytes {
        return untrustworthy(UntrustworthyReason::Indeterminate {
            detail: format!(
                "provider input exceeds {} byte limit",
                options.resource_limits.max_provider_input_bytes
            ),
        });
    }
    if options.cancellation.is_cancelled() {
        return untrustworthy(UntrustworthyReason::Cancelled);
    }
    let started = std::time::Instant::now();
    let result = provider.analyze(EmbeddedProviderRequest {
        gate,
        baseline,
        candidate,
        policy_bytes,
        cancellation: &options.cancellation,
        limits: &options.resource_limits,
    });
    let result = match result {
        Ok(result) => result,
        Err(EmbeddedProviderError::Cancelled) => {
            return untrustworthy(UntrustworthyReason::Cancelled);
        }
        Err(EmbeddedProviderError::IncompatibleArtifact(detail)) => {
            return untrustworthy(UntrustworthyReason::ProviderArtifactIncompatible {
                provider: provider_id.to_string(),
                detail,
            });
        }
        Err(error) => {
            return untrustworthy(UntrustworthyReason::Indeterminate {
                detail: format!("embedded provider {provider_id:?} failed: {error}"),
            });
        }
    };
    let origin = Origin::Embedded {
        provider: provider_id.to_string(),
        version: result.implementation_version.clone(),
        analyzer_fingerprint: result.analyzer_fingerprint.clone(),
    };
    let mut builder = GateRunBuilder::new();
    let required_covered = result
        .coverage
        .required_paths
        .is_subset(&result.coverage.inspected_paths);
    let declared_paths_exist = result
        .coverage
        .inspected_paths
        .iter()
        .all(|path| baseline.entry(path).is_some() || candidate.entry(path).is_some());
    let coverage_complete = result.coverage.complete && required_covered && declared_paths_exist;
    builder.coverage(rulebound_orchestrate::GateCoverage {
        inspected_paths: result.coverage.inspected_paths.iter().cloned().collect(),
        required_paths: result.coverage.required_paths.iter().cloned().collect(),
        complete: coverage_complete,
    });
    if options.cancellation.is_cancelled() {
        builder.untrustworthy(UntrustworthyReason::Cancelled);
    } else if started.elapsed() > options.resource_limits.max_elapsed {
        builder.untrustworthy(UntrustworthyReason::Indeterminate {
            detail: "embedded provider exceeded its elapsed-time limit".to_string(),
        });
    }
    if result.findings.len() > options.resource_limits.max_findings {
        builder.untrustworthy(UntrustworthyReason::Indeterminate {
            detail: format!(
                "embedded provider exceeded its {} finding limit",
                options.resource_limits.max_findings
            ),
        });
    }
    if !coverage_complete {
        builder.untrustworthy(UntrustworthyReason::ProviderCoverageIncomplete {
            provider: provider_id.to_string(),
            detail: "provider did not inspect every path required for complete coverage"
                .to_string(),
        });
    }
    if result.implementation_version.is_empty()
        || result.analyzer_identity.is_empty()
        || result.analyzer_fingerprint.is_empty()
        || result.effective_policy_fingerprint.is_empty()
    {
        builder.untrustworthy(UntrustworthyReason::Indeterminate {
            detail: "provider returned incomplete provenance".to_string(),
        });
    }
    for finding in result.findings {
        if finding
            .line
            .is_some_and(|line| line == 0 || line == u32::MAX)
        {
            builder.untrustworthy(UntrustworthyReason::Indeterminate {
                detail: "provider returned an invalid source line".to_string(),
            });
            continue;
        }
        if finding.rule_id.is_empty() || finding.fingerprint.is_empty() {
            builder.untrustworthy(UntrustworthyReason::Indeterminate {
                detail: "provider returned a finding without rule identity or fingerprint"
                    .to_string(),
            });
            continue;
        }
        let path = match finding.path.as_deref() {
            Some(path) if candidate.entry(path).is_some() => {
                match camino::Utf8PathBuf::from_path_buf(path.into()) {
                    Ok(path) => Some(path),
                    Err(_) => {
                        builder.untrustworthy(UntrustworthyReason::Indeterminate {
                            detail: "provider returned a non-UTF-8 location".to_string(),
                        });
                        continue;
                    }
                }
            }
            Some(path) => {
                builder.untrustworthy(UntrustworthyReason::Indeterminate {
                    detail: format!(
                        "provider returned location outside candidate snapshot: {path:?}"
                    ),
                });
                continue;
            }
            None => None,
        };
        let related_locations: Vec<RelatedLocation> = finding.related_locations;
        if related_locations.iter().any(|location| {
            candidate.entry(&location.path).is_none()
                || location.start_line.is_some_and(|line| line == 0)
                || location.end_line.is_some_and(|line| line == 0)
                || location.start_line.is_some_and(|line| line == u32::MAX)
                || location.end_line.is_some_and(|line| line == u32::MAX)
                || matches!((location.start_line, location.end_line), (Some(start), Some(end)) if end < start)
        })
        {
            builder.untrustworthy(UntrustworthyReason::Indeterminate {
                detail: "provider returned a related location outside candidate snapshot"
                    .to_string(),
            });
            continue;
        }
        let hunk = finding.line.map(|line| HunkRef {
            start: line,
            end: line,
        });
        let expected = finding.expected.map_or(Side::Absent, Side::Value);
        let observed = finding.observed.map_or(Side::Absent, Side::Value);
        let metadata = EmbeddedFindingMetadata {
            rule_id: finding.rule_id.clone(),
            source_fingerprint: finding.fingerprint,
            source_severity: finding.source_severity,
            analyzer_identity: result.analyzer_identity.clone(),
            effective_policy_fingerprint: result.effective_policy_fingerprint.clone(),
            related_locations,
        };
        let mapped = rulebound_orchestrate::Finding::new(
            gate.id.clone(),
            gate.primitive,
            gate.severity,
            Subject {
                kind: SubjectKind::EmbeddedRule,
                name: finding.rule_id,
            },
            path,
            hunk,
            expected,
            observed,
            finding.message,
            origin.clone(),
        )
        .with_embedded_metadata(metadata);
        builder.finding(mapped);
    }
    builder.finish(gate.id.clone(), gate.primitive, origin)
}

fn timeout(gate: &Gate) -> Duration {
    Duration::from_secs(gate.timeout_seconds.unwrap_or(900))
}
