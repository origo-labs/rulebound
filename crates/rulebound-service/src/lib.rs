//! Shared evaluation orchestration for the Rulebound CLI and embedded callers.
//!
//! This first service slice accepts the existing bounded observation model.
//! Snapshot capture and execution-root verification will be added at this
//! boundary while the CLI remains an adapter over the shared evaluator.

use std::path::PathBuf;
use std::time::Duration;

use rulebound_contract::{Contract, Gate, GateId, Primitive, Severity};
use rulebound_gates::{GateContext, GateResult, registry};
use rulebound_observe::Observation;
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
    }
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
