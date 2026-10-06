# Continuous integration

Install Rulebound in the CI job, run `check`, and preserve the exit status so a blocking finding or an untrustworthy run is visible. For GitHub Actions, a minimal workflow step is:

```yaml
- name: Install Rulebound
  run: cargo install --git https://github.com/origo-labs/rulebound rulebound-cli

- name: Check contract
  run: rulebound check --format sarif > results.sarif
```

To upload the SARIF report even when Rulebound returns a non-zero status, let the report step continue and upload afterward:

```yaml
- name: Check contract
  run: rulebound check --format sarif > results.sarif
  continue-on-error: true

- name: Upload SARIF
  if: always()
  uses: github/codeql-action/upload-sarif@v3
  with:
    sarif_file: results.sarif
```

The sample installs the latest revision from the Git repository. For repeatable CI, pin the install to a reviewed tag or commit supported by Cargo's `--rev` option, and manage the installed version as part of your toolchain.

## Baseline selection

Set `[baseline].ref` in `rulebound.toml` to the branch or commit that represents the change's base, or pass `--base <ref>` to `rulebound check`. Rulebound uses `HEAD` as the proposed head. Ensure the CI checkout includes the selected base commit; a shallow checkout that omits it cannot provide a trustworthy comparison.

For a pull request, use the target branch's merge base or the event's base commit as appropriate for your checkout strategy. Choose one reference consistently: two-tree checks report changes relative to it, and a moving or incorrect base can change the findings.

## Third-party checks

An `external_tool` gate runs the configured executable with its argument list. The executable must be installed in CI. If it requires a baseline artifact, arrange for CI to build or retrieve an artifact bound to the selected base commit and pass its path through the configured arguments. Refresh it when the base changes; the third-party tool's stale or missing baseline should result in an error, not an assumed pass.

See [Reports and exit codes](reports.md) for interpreting failures and [Configuration](configuration.md) for the external tool fields.
