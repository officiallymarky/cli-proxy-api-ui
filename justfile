# Run the project's checks. `verify` is the project's own gate.
# >>> guardrails >>>
# Installed by ~/.agents/skills/project-guardrails. Re-run the installer to
# refresh; do not edit this block by hand.

# Secret scan of the working tree, including untracked files. The size cap
# keeps build trees out of the scan; the policy allowlists build output.
secrets:
    betterleaks dir . --config .betterleaks.toml --redact --validation=false --max-target-megabytes 1

# Dependency scan against the offline OSV database.
vulns:
    osv-scanner scan source --offline --no-resolve --no-call-analysis=go -r .

# Static analysis over the vendored ruleset, scoped to the repository.
sast:
    semgrep scan --config ~/.agents/semgrep/rules --metrics=off --disable-version-check --error -j 1 .
# <<< guardrails <<<
