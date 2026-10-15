//! Shared evaluation orchestration for the Rulebound CLI and embedded callers.
//!
//! This crate owns shared evaluation orchestration. The CLI adapts its current
//! Git-backed observation flow, while embedded callers can provide explicit
//! snapshots through the validated snapshot request API.

use std::path::PathBuf;
use std::time::Duration;
use std::{error::Error, fmt};

use rulebound_contract::{Contract, Gate, GateId, Primitive, Severity};
use rulebound_gates::{GateContext, GateResult, registry};
use rulebound_observe::snapshot::{Snapshot, SnapshotError, SnapshotIdentity};
use rulebound_observe::{Budget, Observation, SnapshotObservationError};
use rulebound_orchestrate::{GateRun, ReductionInput, Verdict, reduce};

/// Inputs that the caller supplies to make an evaluation reproducible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluationOptions {
    /// Root directory in which all process checks run.
    pub execution_root: PathBuf,
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
}

impl<'a> SnapshotEvaluationRequest<'a> {
    /// Start building an explicit snapshot evaluation request.
    pub fn builder(
        contract: &'a Contract,
        contract_bytes: &'a [u8],
        baseline: &'a Snapshot,
        candidate: &'a Snapshot,
        options: &'a EvaluationOptions,
    ) -> SnapshotEvaluationRequestBuilder<'a> {
        SnapshotEvaluationRequestBuilder {
            contract,
            contract_bytes,
            baseline,
            candidate,
            options,
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
            let (run, provided_inventory) = rulebound_exec::checks_green::run_checks_in(
                rulebound_exec::origin("cargo", &options.cargo_version),
                timeout(gate),
                &options.execution_root,
            );
            runs.push(run);
            inventory = Some(provided_inventory);
        }
    }

    for gate in &contract.gates {
        if gate.primitive == Primitive::ChecksGreen && inventory.is_some() {
            continue;
        }
        runs.push(run_gate(gate, observation, options, inventory.as_ref()));
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
    let mut evaluation = evaluate(request.contract, &observation, request.options);
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
        run_gate(&gate, observation, options, None)
    })
    .collect()
}

fn run_gate(
    gate: &Gate,
    observation: &Observation,
    options: &EvaluationOptions,
    inventory: Option<&rulebound_exec::test_inventory::TestInventory>,
) -> GateRun {
    if gate.severity == Severity::Off {
        return GateRun::skipped(gate.id.clone(), gate.primitive);
    }

    match gate.primitive {
        Primitive::ChecksGreen => {
            rulebound_exec::checks_green::run_checks_in(
                rulebound_exec::origin("cargo", &options.cargo_version),
                timeout(gate),
                &options.execution_root,
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
            rulebound_exec::external_tool::run_tool_in(
                &gate.id,
                tool,
                &gate.args,
                format,
                &values,
                timeout(gate),
                rulebound_exec::origin(tool, &options.external_tool_version),
                &options.execution_root,
            )
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

fn timeout(gate: &Gate) -> Duration {
    Duration::from_secs(gate.timeout_seconds.unwrap_or(900))
}
