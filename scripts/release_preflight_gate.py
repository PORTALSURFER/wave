#!/usr/bin/env python3
"""Verify the protected WAVE release-preflight evidence for publication."""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
from dataclasses import dataclass
from typing import Any, Mapping
from urllib.error import HTTPError, URLError
from urllib.parse import urlencode
from urllib.request import Request, urlopen


WORKFLOW_PATH = ".github/workflows/release-preflight.yml"
WORKFLOW_NAME = "WAVE release preflight"
WORKFLOW_EVENT = "push"
WORKFLOW_BRANCH = "main"
PUBLISHER_JOB = "publisher_integration"
PUBLISHER_ENVIRONMENT = "publisher-integration"
API_VERSION = "2022-11-28"
GIT_SHA = re.compile(r"[0-9a-f]{40}\Z")
REPOSITORY = re.compile(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+\Z")


class GateError(ValueError):
    """Raised when the external release evidence cannot be trusted."""


@dataclass(frozen=True)
class GateEvidence:
    run_id: int
    run_attempt: int
    job_id: int
    approver: str


def _object(value: Any, label: str) -> Mapping[str, Any]:
    if not isinstance(value, dict):
        raise GateError(f"{label} must be an object")
    return value


def _string(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value:
        raise GateError(f"{label} must be a non-empty string")
    return value


def _positive_int(value: Any, label: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
        raise GateError(f"{label} must be a positive integer")
    return value


def _validate_source_sha(source_sha: Any) -> str:
    if not isinstance(source_sha, str) or not GIT_SHA.fullmatch(source_sha):
        raise GateError("prepared source SHA is invalid")
    return source_sha


def _validate_run_identity(run: Any, source_sha: str, label: str) -> tuple[int, int, str]:
    source_sha = _validate_source_sha(source_sha)
    document = _object(run, label)
    expected = {
        "name": WORKFLOW_NAME,
        "path": WORKFLOW_PATH,
        "event": WORKFLOW_EVENT,
        "head_branch": WORKFLOW_BRANCH,
        "head_sha": source_sha,
        "status": "completed",
    }
    for field, expected_value in expected.items():
        if document.get(field) != expected_value:
            raise GateError(f"{label} has invalid {field}")
    conclusion = _string(document.get("conclusion"), f"{label}.conclusion")
    return (
        _positive_int(document.get("id"), f"{label}.id"),
        _positive_int(document.get("run_attempt"), f"{label}.run_attempt"),
        conclusion,
    )


def parse_successful_run(runs_document: Any, source_sha: str) -> tuple[int, int]:
    """Select the newest exact completed workflow run and require success."""
    source_sha = _validate_source_sha(source_sha)
    document = _object(runs_document, "workflow runs response")
    runs = document.get("workflow_runs")
    if not isinstance(runs, list) or not runs:
        raise GateError("workflow runs response has no runs")

    candidates: list[tuple[int, int, str]] = []
    for index, run in enumerate(runs):
        candidates.append(_validate_run_identity(run, source_sha, f"workflow run {index}"))
    run_id, run_attempt, conclusion = max(candidates, key=lambda candidate: (candidate[0], candidate[1]))
    if conclusion != "success":
        raise GateError("latest exact-SHA workflow run did not succeed")
    return run_id, run_attempt


def parse_successful_attempt(attempt_document: Any, *, source_sha: str, run_id: int, run_attempt: int) -> None:
    """Revalidate the exact latest attempt selected from the run list."""
    source_sha = _validate_source_sha(source_sha)
    run_id = _positive_int(run_id, "selected workflow run.id")
    run_attempt = _positive_int(run_attempt, "selected workflow run.run_attempt")
    actual_run_id, actual_attempt, conclusion = _validate_run_identity(attempt_document, source_sha, "workflow run attempt")
    if actual_run_id != run_id or actual_attempt != run_attempt:
        raise GateError("workflow run attempt does not match the selected run")
    if conclusion != "success":
        raise GateError("selected workflow run attempt did not succeed")


def parse_publisher_job(jobs_document: Any, *, source_sha: str, run_id: int, run_attempt: int) -> int:
    """Require exactly one successful publisher job from the selected attempt."""
    source_sha = _validate_source_sha(source_sha)
    run_id = _positive_int(run_id, "selected workflow run.id")
    run_attempt = _positive_int(run_attempt, "selected workflow run.run_attempt")
    document = _object(jobs_document, "workflow jobs response")
    jobs = document.get("jobs")
    if not isinstance(jobs, list) or not jobs:
        raise GateError("workflow jobs response has no jobs")

    matching: list[Mapping[str, Any]] = []
    for index, job in enumerate(jobs):
        job_document = _object(job, f"workflow job {index}")
        if job_document.get("name") == PUBLISHER_JOB:
            matching.append(job_document)
    if len(matching) != 1:
        raise GateError("workflow attempt does not contain exactly one publisher_integration job")

    job = matching[0]
    expected = {
        "name": PUBLISHER_JOB,
        "status": "completed",
        "conclusion": "success",
        "head_sha": source_sha,
        "head_branch": WORKFLOW_BRANCH,
        "run_id": run_id,
        "run_attempt": run_attempt,
        "workflow_name": WORKFLOW_NAME,
    }
    for field, expected_value in expected.items():
        if job.get(field) != expected_value:
            raise GateError(f"publisher_integration job has invalid {field}")
    if job.get("environment") not in (None, PUBLISHER_ENVIRONMENT):
        raise GateError("publisher_integration job has an unexpected environment")
    return _positive_int(job.get("id"), "publisher_integration job.id")


def parse_publisher_approval(approvals_document: Any) -> str:
    """Require a recorded approval for the protected publisher environment."""
    if not isinstance(approvals_document, list) or not approvals_document:
        raise GateError("workflow run has no environment approval history")

    approvers: list[str] = []
    for index, approval in enumerate(approvals_document):
        approval_document = _object(approval, f"workflow approval {index}")
        state = _string(approval_document.get("state"), f"workflow approval {index}.state")
        environments = approval_document.get("environments")
        if not isinstance(environments, list) or not environments:
            raise GateError(f"workflow approval {index} has no environments")
        user = _object(approval_document.get("user"), f"workflow approval {index}.user")
        approver = _string(user.get("login"), f"workflow approval {index}.user.login")

        target_environment_found = False
        for environment_index, environment in enumerate(environments):
            environment_document = _object(environment, f"workflow approval {index}.environment {environment_index}")
            environment_name = _string(
                environment_document.get("name"),
                f"workflow approval {index}.environment {environment_index}.name",
            )
            _positive_int(
                environment_document.get("id"),
                f"workflow approval {index}.environment {environment_index}.id",
            )
            if environment_name == PUBLISHER_ENVIRONMENT:
                target_environment_found = True

        if target_environment_found:
            if state != "approved":
                raise GateError("publisher-integration environment approval is not approved")
            approvers.append(approver)

    if not approvers:
        raise GateError("workflow run has no publisher-integration environment approval")
    return approvers[-1]


def validate_evidence(
    runs_document: Any,
    attempt_document: Any,
    jobs_document: Any,
    approvals_document: Any,
    *,
    source_sha: str,
) -> GateEvidence:
    """Parse and cross-check all API evidence needed before production work."""
    run_id, run_attempt = parse_successful_run(runs_document, source_sha)
    parse_successful_attempt(attempt_document, source_sha=source_sha, run_id=run_id, run_attempt=run_attempt)
    job_id = parse_publisher_job(jobs_document, source_sha=source_sha, run_id=run_id, run_attempt=run_attempt)
    approver = parse_publisher_approval(approvals_document)
    return GateEvidence(run_id=run_id, run_attempt=run_attempt, job_id=job_id, approver=approver)


def _api_json(api_base: str, repository: str, path: str, token: str, **params: str) -> Any:
    query = urlencode(params)
    url = f"{api_base.rstrip('/')}/repos/{repository}/{path}"
    if query:
        url = f"{url}?{query}"
    request = Request(
        url,
        headers={
            "Accept": "application/vnd.github+json",
            "Authorization": f"Bearer {token}",
            "X-GitHub-Api-Version": API_VERSION,
        },
    )
    try:
        with urlopen(request, timeout=20) as response:
            body = response.read()
    except HTTPError as error:
        raise GateError(f"GitHub Actions API request failed with HTTP {error.code}") from error
    except (URLError, OSError) as error:
        raise GateError("GitHub Actions API request failed") from error
    try:
        return json.loads(body.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise GateError("GitHub Actions API returned malformed JSON") from error


def fetch_evidence(*, api_base: str, repository: str, source_sha: str, token: str) -> GateEvidence:
    """Fetch only read-only Actions API evidence and validate its identity."""
    source_sha = _validate_source_sha(source_sha)
    if not isinstance(repository, str) or not REPOSITORY.fullmatch(repository):
        raise GateError("GitHub repository is invalid")
    if not isinstance(token, str) or not token.strip():
        raise GateError("workflow token is missing")
    runs_document = _api_json(
        api_base,
        repository,
        f"actions/workflows/{WORKFLOW_PATH.rsplit('/', 1)[-1]}/runs",
        token,
        event=WORKFLOW_EVENT,
        branch=WORKFLOW_BRANCH,
        head_sha=source_sha,
        status="completed",
        per_page="100",
    )
    run_id, run_attempt = parse_successful_run(runs_document, source_sha)
    attempt_document = _api_json(
        api_base,
        repository,
        f"actions/runs/{run_id}/attempts/{run_attempt}",
        token,
    )
    jobs_document = _api_json(
        api_base,
        repository,
        f"actions/runs/{run_id}/attempts/{run_attempt}/jobs",
        token,
        per_page="100",
    )
    approvals_document = _api_json(
        api_base,
        repository,
        f"actions/runs/{run_id}/approvals",
        token,
    )
    return validate_evidence(
        runs_document,
        attempt_document,
        jobs_document,
        approvals_document,
        source_sha=source_sha,
    )


def _build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repository", default=os.environ.get("GITHUB_REPOSITORY"))
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--api-base", default="https://api.github.com")
    return parser


def main(argv: list[str] | None = None) -> int:
    args = _build_parser().parse_args(argv)
    try:
        if not isinstance(args.repository, str) or not REPOSITORY.fullmatch(args.repository):
            raise GateError("GitHub repository is invalid")
        token = os.environ.get("GITHUB_TOKEN")
        if not token:
            raise GateError("workflow token is missing")
        evidence = fetch_evidence(
            api_base=args.api_base,
            repository=args.repository,
            source_sha=args.source_sha,
            token=token,
        )
    except (GateError, OSError) as error:
        print(f"publisher preflight gate failed closed: {error}", file=sys.stderr)
        return 1
    print(
        "publisher preflight gate passed: "
        f"run {evidence.run_id} attempt {evidence.run_attempt}, "
        f"job {evidence.job_id}, approver {evidence.approver}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
