//! `rulebound` — the single binary.
//!
//! M0 has no gates registered, so `check` returns `accept` and says so plainly
//! rather than implying the repository was inspected. `observe` is the
//! milestone's real deliverable: it prints the bounded observation a supervisor
//! would act on, which is how the empty-diff class of bug becomes visible
//! during development rather than after a measurement programme has run on it.
//!
//! M3 adds `init`, the real contract loader, and the report formats.

// Writing to stdout is this crate's product, not its logging: `rulebound
// observe` *is* an observation printer, and `rulebound check` emits a report
// designed to be piped. `print_stdout` is denied workspace-wide for exactly
// that reason and re-permitted here and only here, where the output is the
// artefact rather than a debugging habit.
#![allow(clippy::print_stdout)]

use std::path::PathBuf;
use std::process::ExitCode;

use camino::Utf8PathBuf;
use clap::{Parser, Subcommand};

use rulebound_contract::Contract;
use rulebound_git::Repo;
use rulebound_observe::{Budget, Observation};
use rulebound_orchestrate::Verdict;
use rulebound_report::Report;
use rulebound_service::{EvaluationOptions, evaluate};

#[derive(Debug, Parser)]
#[command(
    name = "rulebound",
    about = "A supervisor for coding agents that runs a project's declared quality contract.",
    version
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Run the declared contract and return a verdict.
    Check {
        /// Repository to inspect. Defaults to the working directory.
        #[arg(long)]
        path: Option<PathBuf>,
        /// Baseline commit-ish. Overrides `[baseline].ref` in the contract.
        #[arg(long)]
        base: Option<String>,
        /// Observation budget in bytes. Overrides `[budget]`.
        #[arg(long)]
        budget: Option<usize>,
        /// Report format.
        #[arg(long, value_enum, default_value_t = Format::Human)]
        format: Format,
    },
    /// Write a starting `rulebound.toml`.
    ///
    /// The product's main act. Every gate is written at `warn`: this is a
    /// proposal, not a verdict, and a contract written today has been
    /// calibrated on nothing.
    Init {
        /// Repository to inspect. Defaults to the working directory.
        #[arg(long)]
        path: Option<PathBuf>,
        /// Overwrite an existing contract. Off by default, because silently
        /// replacing the thing somebody reviewed is the one thing this command
        /// must never do.
        #[arg(long)]
        force: bool,
        /// Print the contract to stdout instead of writing it.
        #[arg(long)]
        dry_run: bool,
    },
    /// Print the bounded observation. Exists so the empty-diff failure mode
    /// is visible during development, not after it has been measured on.
    Observe {
        #[arg(long)]
        path: Option<PathBuf>,
        #[arg(long, default_value = "HEAD")]
        base: String,
        #[arg(long, default_value_t = Budget::DEFAULT.bytes())]
        budget: usize,
        /// Assert that a change is expected, so an empty observation is
        /// reported as a fault rather than as a clean tree.
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        expect_diff: bool,
    },
}

/// The report format. `human` is the default because a supervisor's first
/// audience is a person deciding whether to read the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum Format {
    /// A PR comment.
    Human,
    /// The machine contract, with the three targets as separate top-level keys.
    Json,
    /// SARIF 2.1.0, for code scanning.
    Sarif,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(verdict) => ExitCode::from(verdict.exit_code() as u8),
        Err(msg) => {
            // A startup failure is `error`, never `block`: a verifier that
            // could not start and a verifier that failed are different
            // events, and conflating them is the bug this project exists to
            // avoid (EVIDENCE.md 5, PRD 4).
            eprintln!("rulebound: {msg}");
            ExitCode::from(Verdict::Error.exit_code() as u8)
        }
    }
}

fn run(cli: Cli) -> Result<Verdict, String> {
    // `expect_diff` defaults to true for `check`: a supervisor that was asked
    // to review a change and observed nothing must say so, not report a clean
    // tree. `observe --expect-diff=false` is the escape hatch for inspecting
    // a repository with no pending work.
    let observing = matches!(cli.cmd, Cmd::Observe { .. });
    let initialising = matches!(cli.cmd, Cmd::Init { .. });
    let format = match cli.cmd {
        Cmd::Check { format, .. } => format,
        Cmd::Observe { .. } | Cmd::Init { .. } => Format::Human,
    };
    if initialising {
        return run_init(cli);
    }

    let (path, cli_base, cli_budget, expect_diff) = match cli.cmd {
        Cmd::Check {
            path, base, budget, ..
        } => (path, base, budget, true),
        Cmd::Observe {
            path,
            base,
            budget,
            expect_diff,
        } => (path, Some(base), Some(budget), expect_diff),
        Cmd::Init { path, .. } => (path, None, None, false),
    };

    let start: PathBuf = path.unwrap_or_else(|| PathBuf::from("."));
    let start = start
        .into_os_string()
        .into_string()
        .map_err(|p| format!("path is not UTF-8: {p:?}"))?;
    let start = Utf8PathBuf::from(start);
    let repo = Repo::open(&start).map_err(|e| e.to_string())?;

    // `observe` is a debugging tool and deliberately contract-free: its job is
    // to show what a supervisor would see, including on a repository that has
    // not declared a contract yet.
    if observing {
        let base = cli_base.unwrap_or_else(|| "HEAD".to_string());
        let budget = resolve_budget(cli_budget, None)?;
        let obs = capture(&repo, &base, budget, expect_diff)?;
        print_observation(&obs);
        println!("verdict: {}", Verdict::Accept);
        return Ok(Verdict::Accept);
    }

    // `check` is the product. A missing or invalid contract is `error`, and
    // never `block`: a contract that could not be read has not been violated,
    // and conflating the two is the bug this project exists to avoid.
    let contract = load_contract(repo.root())?;
    let base = cli_base
        .or_else(|| contract.baseline_ref.clone())
        .unwrap_or_else(|| "HEAD".to_string());
    let budget = resolve_budget(cli_budget, Some(&contract))?;

    let obs = capture(&repo, &base, budget, true)?;
    let (cargo_version, tool_version) = tool_versions();
    let evaluation = evaluate(
        &contract,
        &obs,
        &EvaluationOptions {
            execution_root: repo.root().as_std_path().to_path_buf(),
            environment: rulebound_exec::EnvironmentPolicy::inherit_all(),
            baseline: base,
            candidate: "HEAD".to_string(),
            now_unix: unix_now(),
            cargo_version,
            external_tool_version: tool_version,
            cancellation: rulebound_service::CancellationToken::default(),
            resource_limits: rulebound_service::ResourceLimits::default(),
        },
    );

    let report = Report {
        contract: &contract,
        runs: &evaluation.runs,
        verdict: evaluation.verdict,
        tool_version: env!("CARGO_PKG_VERSION"),
    };
    // The report goes to stdout so it can be piped; anything a human needs
    // that is *not* the report goes to stderr.
    print!(
        "{}",
        match format {
            Format::Human => report.human(),
            Format::Json => report.json(),
            Format::Sarif => report.sarif(),
        }
    );
    Ok(evaluation.verdict)
}

/// Write a starting contract.
///
/// # Errors
///
/// Returns a message rather than overwriting when a contract already exists,
/// unless `force` is given. Overwriting the artefact somebody reviewed is the
/// one thing this command must never do by accident: a contract is a dated
/// claim about a project's bar, and silently replacing it is a way of losing
/// the review that made it true.
fn run_init(cli: Cli) -> Result<Verdict, String> {
    let (path, force, dry_run) = match cli.cmd {
        Cmd::Init {
            path,
            force,
            dry_run,
        } => (path, force, dry_run),
        _ => return Err("init was not the subcommand".to_string()),
    };
    let start: PathBuf = path.unwrap_or_else(|| PathBuf::from("."));
    let start = start
        .into_os_string()
        .into_string()
        .map_err(|p| format!("path is not UTF-8: {p:?}"))?;
    let root = Utf8PathBuf::from(start);
    let repo = Repo::open(&root).map_err(|e| format!("{root}: {e}"))?;

    let target = repo
        .root()
        .join(rulebound_contract::parse::CONTRACT_FILENAME);
    if target.exists() && !force && !dry_run {
        return Err(format!(
            "{} already exists. A contract is a dated claim about this \
             project's bar; replacing it silently would lose the review that \
             made it true. Re-run with --force if you mean it, or --dry-run to \
             see what would be written.",
            target
        ));
    }

    // A review date of today, because a project that has just generated a
    // contract genuinely has just reviewed it. Anything else would be a
    // fabricated date, and `contract_review_stale` would eventually fire on
    // the fabrication rather than on the staleness.
    let reviewed = utc_today();
    let members = workspace_members(repo.root());
    let contract = rulebound_contract::generate::generate(&reviewed, members);

    // Round-trip before writing. A generator that emits plausible TOML the
    // parser rejects is worse than no generator, and the failure would land on
    // whoever ran it.
    rulebound_contract::parse::parse_contract(&contract).map_err(|e| {
        format!(
            "refusing to write a contract this build cannot read back: {e}. \
             That is a bug in `rulebound init`."
        )
    })?;

    if dry_run {
        print!("{contract}");
        return Ok(Verdict::Accept);
    }
    std::fs::write(&target, &contract).map_err(|e| format!("could not write {target}: {e}"))?;
    eprintln!("wrote {target}");
    eprintln!(
        "Every gate is at `warn`: this proposes, it does not judge. Run \
         `rulebound check` to see what it reports, and promote a gate to \
         `error` only once you have a reason and a measurement."
    );
    Ok(Verdict::Accept)
}

/// `YYYY-MM-DD` for today, UTC.
fn utc_today() -> String {
    let secs = unix_now();
    // Days since the Unix epoch, to a civil date. Hinnant's algorithm, the
    // same one `JudgementSection` uses, kept in step deliberately.
    let days = secs.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// How many crates a workspace declares, so the generated contract can say so.
///
/// Read from `Cargo.toml` rather than by running cargo: `init` should not
/// build anything, and a member count is a fact about a file.
fn workspace_members(root: &Utf8PathBuf) -> usize {
    let manifest = root.join("Cargo.toml");
    let Ok(text) = std::fs::read_to_string(&manifest) else {
        return 0;
    };
    // A tiny targeted read rather than a TOML dependency here: the only thing
    // needed is the length of `[workspace] members`, and a malformed manifest
    // should not fail `init` -- a project that cannot be introspected can still
    // be given a contract.
    let Ok(value) = text.parse::<toml::Table>() else {
        return 0;
    };
    value
        .get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(|m| m.as_array())
        .map_or(0, |m| m.len())
}

fn capture(
    repo: &Repo,
    base: &str,
    budget: Budget,
    expect_diff: bool,
) -> Result<Observation, String> {
    let inputs = repo
        .observation_inputs(Some(base))
        .map_err(|e| format!("could not observe: {e}"))?;
    Ok(Observation::capture(&inputs, budget, expect_diff))
}

fn resolve_budget(cli: Option<usize>, contract: Option<&Contract>) -> Result<Budget, String> {
    let bytes = cli.unwrap_or_else(|| {
        contract.map_or(rulebound_contract::DEFAULT_OBSERVATION_BYTES, |c| {
            c.budget_observation_bytes
        })
    });
    Budget::new(bytes).ok_or_else(|| {
        format!(
            "observation budget {bytes} is below the minimum of {}; a truncated \
             observation that small is not evidence of anything",
            Budget::MIN
        )
    })
}

/// Read and validate `rulebound.toml` from the repository root.
fn load_contract(root: &Utf8PathBuf) -> Result<Contract, String> {
    let path = root.join(rulebound_contract::parse::CONTRACT_FILENAME);
    let source = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "could not read {}: {e}. A repository without a contract has no \
             declared quality bar, and a supervisor cannot invent one.",
            path
        )
    })?;
    rulebound_contract::parse::parse_contract(&source).map_err(|e| format!("{path}: {e}"))
}

/// Unix seconds, once per invocation.
///
/// The only clock in the workspace. Every gate that needs the time is handed
/// it, so that no gate's output depends on when it happened to run.
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// The versions of the tools we delegate to, resolved once per invocation.
///
/// Asking a tool its version is running a process, so it goes through
/// `rulebound-exec`. The CLI used to shell out inline and
/// `scripts/check-boundary.sh` caught it, which is the check earning its
/// place.
fn tool_versions() -> (String, String) {
    use std::sync::OnceLock;
    static CACHE: OnceLock<(String, String)> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            (
                rulebound_exec::tool_version("cargo"),
                rulebound_exec::tool_version("slop-gate"),
            )
        })
        .clone()
}

fn print_observation(obs: &Observation) {
    println!("base:          {}", obs.base.as_deref().unwrap_or("<none>"));
    println!("budget:        {} bytes", obs.budget.bytes());
    println!(
        "observation:   {}{}",
        if obs.diff.truncated {
            "truncated"
        } else {
            "intact"
        },
        match obs.empty {
            Some(reason) => format!(" (empty: {reason})"),
            None => String::new(),
        }
    );
    println!("files:         {}", obs.files.len());
    for f in &obs.files {
        println!(
            "  {} (base: {}, head: {})",
            f.path,
            if f.base.is_some() { "yes" } else { "no" },
            if f.head.is_some() { "yes" } else { "no" }
        );
    }
    println!("--- diff ---\n{}", obs.diff.text);
}
