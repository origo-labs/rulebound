# Rulebound user guide

Rulebound runs a project's declared quality contract against a change and reports whether the change passed, should be blocked, needs a human, or could not be assessed. It does not decide whether the code is good. The contract describes what the project wants checked, and Rulebound runs the checks it supports.

Start with [`getting-started.md`](getting-started.md) to install Rulebound and run it for the first time. Then review [`configuration.md`](configuration.md) to understand and maintain `rulebound.toml`. The remaining sections describe the available checks, interpreting results, and using Rulebound in CI.

## Contents

- [Getting started](getting-started.md): install, initialize a contract, and run your first check.
- [Configuration](configuration.md): contract fields, severity, baselines, and documented changes.
- [Checks](checks.md): what each supported gate inspects and its limits.
- [Reports and exit codes](reports.md): read human, JSON, and SARIF output and handle verdicts.
- [Continuous integration](ci.md): run Rulebound in CI and publish SARIF results.

Rulebound is beta software. Its checks are deterministic, but they are not complete. Keep the contract's `judgement.not_covered` list current and review the findings before making gates blocking.
