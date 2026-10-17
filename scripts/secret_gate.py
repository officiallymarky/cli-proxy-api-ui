#!/usr/bin/env python3
"""Fail-closed Betterleaks secret gate for this repository.

The hooks in ``.githooks`` are thin wrappers around this script:

    .githooks/pre-commit  ->  python3 scripts/secret_gate.py staged
    .githooks/pre-push    ->  python3 scripts/secret_gate.py push

``staged`` scans the added lines of the index. ``push`` scans the outgoing
history of every non-deletion ref update that Git reports on stdin.

Every failure blocks the Git operation: a missing scanner, an unsupported
scanner version, a missing or invalid configuration file, a malformed push
update, a ref that does not resolve to a commit, and a scanner finding.
Scanner exit statuses are preserved and never masked. The gate performs no
remote operation and never prints remote URLs.
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess
import sys
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Final, NamedTuple

SCANNER: Final = "betterleaks"
SUPPORTED_SERIES: Final = (1, 9)
CONFIG_FILENAME: Final = ".betterleaks.toml"
DELETE_REF: Final = "(delete)"
SCANNER_ENV_PREFIXES: Final = ("BETTERLEAKS_", "GITLEAKS_")
PATH_GIT_ENV: Final = (
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
)
# GIT_PREFIX would re-anchor scanner pathspecs to the hook's subdirectory.
DROPPED_GIT_ENV: Final = ("GIT_PREFIX",)
# Read-only plumbing; never run a user-configured pager, hook, or fsmonitor.
GIT_ARGV: Final = ("git", "--no-pager", "-c", "core.fsmonitor=false")
OID_RE: Final = re.compile(r"\A(?:[0-9a-f]{40}|[0-9a-f]{64})\Z")
ZERO_RE: Final = re.compile(r"\A0+\Z")
VERSION_RE: Final = re.compile(r"\A(\d+)\.(\d+)(?:\.(\d+))?\Z")
# Precondition failures exit 2; findings keep the scanner's own status.
EXIT_GATE_ERROR: Final = 2


class GateError(Exception):
    """A gate precondition failed; the Git operation must be blocked."""


@dataclass(frozen=True)
class PushUpdate:
    """One ref update as reported by Git's pre-push protocol."""

    local_ref: str
    local_oid: str
    remote_ref: str
    remote_oid: str


class PushScan(NamedTuple):
    """The outgoing scan selected for one ref update."""

    log_opts: str
    description: str


@dataclass(frozen=True)
class GateContext:
    """Resolved gate inputs shared by the staged and push scans."""

    repo_root: Path
    config: Path
    executable: str
    version: str
    env: Mapping[str, str]


def _spawn(
    argv: Sequence[str],
    cwd: Path,
    env: Mapping[str, str] | None = None,
    *,
    capture: bool = False,
) -> subprocess.CompletedProcess[str]:
    try:
        return subprocess.run(
            list(argv),
            cwd=str(cwd),
            env=None if env is None else dict(env),
            capture_output=capture,
            text=True,
            check=False,
        )
    except FileNotFoundError as error:
        raise GateError(f"executable not found: {argv[0]}") from error


def _git(args: Sequence[str], cwd: Path) -> subprocess.CompletedProcess[str]:
    return _spawn([*GIT_ARGV, *args], cwd, child_environment(Path.cwd()), capture=True)


def _worktree_root(start: Path) -> Path | None:
    if not start.is_dir():
        return None
    result = _git(["rev-parse", "--show-toplevel"], start)
    if result.returncode != 0:
        return None
    reported = result.stdout.strip()
    return Path(reported).resolve() if reported else None


def resolve_repo_root() -> Path:
    """Return the work tree this gate must scan.

    Git runs hooks from the root of the working tree, so the invocation
    directory wins and a hook invoked through a global ``core.hooksPath``
    scans the repository being committed to rather than the hook's own
    repository. The script's repository is the fallback for direct
    invocations from outside any work tree.
    """
    candidates = (Path.cwd(), Path(__file__).resolve().parents[1])
    for candidate in candidates:
        root = _worktree_root(candidate)
        if root is not None:
            return root
    checked = ", ".join(str(candidate) for candidate in candidates)
    raise GateError(f"cannot locate the Git work tree to scan: {checked}")


def child_environment(invocation_cwd: Path) -> dict[str, str]:
    """Return the environment for scanner and Git child processes.

    Scanner configuration variables are removed so the tracked ``--config``
    file stays the only policy source and no provider opt-in is inherited.
    Relative Git path variables are made absolute so they keep pointing at
    the same repository and index after the working directory changes.
    """
    environment = {
        name: value
        for name, value in os.environ.items()
        if not name.startswith(SCANNER_ENV_PREFIXES)
    }
    environment["GIT_NO_LAZY_FETCH"] = "1"
    environment["GIT_OPTIONAL_LOCKS"] = "0"
    for name in DROPPED_GIT_ENV:
        environment.pop(name, None)
    for name in PATH_GIT_ENV:
        value = environment.get(name)
        if value and not os.path.isabs(value):
            environment[name] = str((invocation_cwd / value).resolve())
    return environment


def resolve_config(repo_root: Path) -> Path:
    config = repo_root / CONFIG_FILENAME
    if not config.is_file():
        raise GateError(f"the tracked scanner configuration is missing: {config}")
    return config


def scanner_executable() -> str:
    found = shutil.which(SCANNER)
    if found is None:
        raise GateError(f"{SCANNER} is not on PATH; the gate cannot scan and blocks")
    return found


def scanner_version(*, executable: str, cwd: Path, env: Mapping[str, str]) -> str:
    result = _spawn([executable, "version"], cwd, env, capture=True)
    if result.returncode != 0:
        raise GateError(f"{SCANNER} version check failed: exit {result.returncode}")
    output = result.stdout.strip()
    lines = output.splitlines()
    reported = lines[0].strip() if lines else ""
    match = VERSION_RE.match(reported)
    if match is None:
        raise GateError(f"unrecognized {SCANNER} version; expected 1.9.x")
    series = (int(match.group(1)), int(match.group(2)))
    if series != SUPPORTED_SERIES:
        raise GateError(
            f"unsupported {SCANNER} version {reported}; this gate supports the "
            f"{SUPPORTED_SERIES[0]}.{SUPPORTED_SERIES[1]}.x series"
        )
    return reported


def check_config(context: GateContext) -> None:
    result = _spawn(
        [context.executable, "config", "check", "--config", str(context.config)],
        context.repo_root,
        context.env,
    )
    if result.returncode != 0:
        raise GateError(
            f"{SCANNER} rejected the gate configuration {context.config}: "
            f"config check exit {result.returncode}"
        )


def build_context(repo_root: Path, env: Mapping[str, str]) -> GateContext:
    """Validate scanner, version, and configuration before any scan."""
    config = resolve_config(repo_root)
    executable = scanner_executable()
    version = scanner_version(executable=executable, cwd=repo_root, env=env)
    context = GateContext(repo_root, config, executable, version, env)
    check_config(context)
    return context


def run_scan(context: GateContext, arguments: Sequence[str]) -> int:
    result = _spawn([context.executable, *arguments], context.repo_root, context.env)
    return result.returncode


def staged_arguments(context: GateContext) -> list[str]:
    return [
        "git",
        str(context.repo_root),
        "--staged",
        "--redact",
        "--validation=false",
        "--config",
        str(context.config),
    ]


def run_staged(repo_root: Path, env: Mapping[str, str]) -> int:
    context = build_context(repo_root, env)
    print(
        f"secret-gate: {SCANNER} {context.version} staged scan of the index "
        f"in {context.repo_root} (config {context.config})",
        file=sys.stderr,
    )
    status = run_scan(context, staged_arguments(context))
    if status != 0:
        print(f"secret-gate: blocking commit (scanner exit {status})", file=sys.stderr)
    return status


def read_push_payload() -> str:
    try:
        return sys.stdin.buffer.read().decode("utf-8")
    except UnicodeDecodeError as error:
        raise GateError("malformed push input: not valid UTF-8") from error


def is_zero_oid(oid: str) -> bool:
    return ZERO_RE.match(oid) is not None


def is_deletion(update: PushUpdate) -> bool:
    return update.local_ref == DELETE_REF or is_zero_oid(update.local_oid)


def parse_push_update(line: str, number: int) -> PushUpdate:
    if not line.strip():
        raise GateError(f"malformed push input on line {number}: empty line")
    fields = line.split()
    if len(fields) != 4:
        raise GateError(
            f"malformed push input on line {number}: "
            f"expected 4 fields, found {len(fields)}"
        )
    local_ref, local_oid, remote_ref, remote_oid = fields
    if not remote_ref.startswith("refs/"):
        raise GateError(f"malformed push input on line {number}: ref {remote_ref!r}")
    for oid in (local_oid, remote_oid):
        if OID_RE.match(oid) is None:
            raise GateError(f"malformed push input on line {number}: oid {oid!r}")
    if local_ref == DELETE_REF and not is_zero_oid(local_oid):
        raise GateError(
            f"malformed push input on line {number}: {DELETE_REF} with object "
            f"{local_oid}"
        )
    return PushUpdate(local_ref, local_oid, remote_ref, remote_oid)


def parse_push_updates(payload: str) -> list[PushUpdate]:
    updates: list[PushUpdate] = []
    for number, line in enumerate(payload.splitlines(), start=1):
        updates.append(parse_push_update(line, number))
    return updates


def object_type(repo_root: Path, oid: str) -> str | None:
    result = _git(["cat-file", "-t", oid], repo_root)
    if result.returncode != 0:
        return None
    kind = result.stdout.strip()
    return kind or None


def peeled_commit(repo_root: Path, oid: str) -> str | None:
    result = _git(["rev-parse", "--verify", "--quiet", f"{oid}^{{commit}}"], repo_root)
    if result.returncode != 0:
        return None
    commit = result.stdout.strip()
    return commit or None


def pushed_tip(repo_root: Path, update: PushUpdate) -> str:
    kind = object_type(repo_root, update.local_oid)
    if kind is None:
        raise GateError(
            f"{update.local_ref} points at {update.local_oid}, which is not "
            "present in this repository"
        )
    if kind not in ("commit", "tag"):
        raise GateError(
            f"{update.local_ref} points at a {kind} object; only commits and "
            "tags are supported"
        )
    commit = peeled_commit(repo_root, update.local_oid)
    if commit is None:
        raise GateError(f"{update.local_ref} does not resolve to a commit")
    return commit


def push_scan(repo_root: Path, update: PushUpdate) -> PushScan:
    """Select the outgoing history to scan for one ref update."""
    tip = pushed_tip(repo_root, update)
    if is_zero_oid(update.remote_oid):
        return PushScan(tip, "new ref, all reachable history")
    base = peeled_commit(repo_root, update.remote_oid)
    if base is None:
        return PushScan(tip, "unknown remote object, all reachable history")
    log_opts = f"{base}..{tip}"
    return PushScan(log_opts, f"range {log_opts}")


def push_arguments(context: GateContext, log_opts: str) -> list[str]:
    return [
        "git",
        str(context.repo_root),
        f"--log-opts={log_opts}",
        "--redact",
        "--validation=false",
        "--config",
        str(context.config),
    ]


def run_push(repo_root: Path, env: Mapping[str, str], payload: str) -> int:
    updates = parse_push_updates(payload)
    published = [update for update in updates if not is_deletion(update)]
    if not published:
        print(
            f"secret-gate: no published ref updates in {len(updates)} update(s); "
            "nothing to scan",
            file=sys.stderr,
        )
        return 0
    shallow = _git(["rev-parse", "--is-shallow-repository"], repo_root)
    if shallow.returncode != 0 or shallow.stdout.strip() != "false":
        raise GateError(
            "outgoing history is incomplete or unavailable (possibly shallow); "
            "restore complete history through an approved workflow before pushing"
        )
    context = build_context(repo_root, env)
    for update in published:
        scan = push_scan(repo_root, update)
        print(
            f"secret-gate: {SCANNER} {context.version} outgoing scan of "
            f"{update.local_ref} ({scan.description})",
            file=sys.stderr,
        )
        status = run_scan(context, push_arguments(context, scan.log_opts))
        if status != 0:
            print(
                f"secret-gate: blocking push of {update.local_ref} "
                f"(scanner exit {status})",
                file=sys.stderr,
            )
            return status
    return 0


def main(argv: Sequence[str]) -> int:
    try:
        if len(argv) != 1 or argv[0] not in ("staged", "push"):
            raise GateError("usage: secret_gate.py staged|push")
        repo_root = resolve_repo_root()
        env = child_environment(Path.cwd())
        if argv[0] == "staged":
            return run_staged(repo_root, env)
        return run_push(repo_root, env, read_push_payload())
    except GateError as error:
        print(f"secret-gate: {error}", file=sys.stderr)
        print("secret-gate: blocked; no scan result is available", file=sys.stderr)
        return EXIT_GATE_ERROR


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
