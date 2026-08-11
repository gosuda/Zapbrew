#!/usr/bin/env python3
"""Verify the Zapbrew stabilization ledger.

Exit 0 means globally complete, 1 means structurally valid but incomplete, and
2 means malformed or contradictory.  The verifier deliberately derives all
coverage, tranche, review, benchmark, and completion state from raw records.
"""

from __future__ import annotations

import argparse
import ast
import base64
import copy
import hashlib
import json
import math
import os
import re
import shutil
import statistics
import sys
import tempfile
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import Any, Callable

SCHEMA_VERSION = 2
HOMEBREW_TARGET = "6.0.16@3ecc9eff23feebf1bc73846d74e14a122c93b66f"
HOMEBREW_COMMIT = "3ecc9eff23feebf1bc73846d74e14a122c93b66f"
HOMEBREW_ANCHOR_PREFIXES = (
    "https://github.com/Homebrew/brew/blob/6.0.16/",
    "https://github.com/Homebrew/brew/tree/6.0.16/",
    f"https://github.com/Homebrew/brew/blob/{HOMEBREW_COMMIT}/",
    f"https://github.com/Homebrew/brew/tree/{HOMEBREW_COMMIT}/",
)
REPOSITORY = "gosuda/Zapbrew"
PRECEDENCE = "constrained-parity"
AUTHORITY = "repository-maintainer"
CANONICAL_FILE_COUNT = 84
CANONICAL_CELL_COUNT = 446
DECISION_IDS = {
    "D2-linux-cask-subset",
    "D3-linux-service-registration",
    "D4-env-contracts",
    "D5-cask-tranche-granularity",
    "D6-completions-surface",
    "D7-output-parity-bars",
    "D8-shared-flag-contracts",
}
PLATFORMS = {"any", "linux", "macos"}
EFFECTS = {
    "value",
    "trust",
    "network",
    "query",
    "filesystem-mutation",
    "native-query",
    "native-mutation",
    "output",
    "refusal",
}
DISPOSITIONS = {
    "must-match",
    "must-be-outside-architecture",
    "product-decision-pending",
    "safety-deviation-candidate",
}
EVIDENCE_KINDS = {
    "platform-independent pure logic",
    "structural-parse",
    "unit-fixture-io",
    "command-construction",
    "cross-compile",
    "process-integration",
    "native-side-effect",
}
FINDINGS = {"none", "missing", "defect"}
COMPLETED_DISPOSITIONS = {
    "proved-matched",
    "proved-outside-architecture",
    "approved-safety-deviation",
}
APPROVAL_KINDS = {
    "scope",
    "product-decision",
    "safety-deviation",
    "performance-floor",
    "hot-reclassification",
    "tranche-review",
}
TRANCHE_STATES = {
    "proposed",
    "scoped",
    "building",
    "evidence-pending",
    "review-pending",
    "approved",
    "changes-requested",
    "landed",
    "blocked",
    "reverted",
}
TRANCHE_EDGES = {
    ("proposed", "scoped"),
    ("scoped", "building"),
    ("scoped", "blocked"),
    ("building", "evidence-pending"),
    ("building", "blocked"),
    ("evidence-pending", "review-pending"),
    ("evidence-pending", "blocked"),
    ("review-pending", "approved"),
    ("review-pending", "changes-requested"),
    ("review-pending", "reverted"),
    ("changes-requested", "building"),
    ("approved", "landed"),
    ("approved", "reverted"),
    ("landed", "reverted"),
    ("blocked", "scoped"),
}
MUTATION_EFFECTS = {"filesystem-mutation", "native-mutation"}
NATIVE_EFFECTS = {"native-query", "native-mutation"}
NATIVE_PROOF_KINDS = {"process-integration", "native-side-effect"}
SHA256_LEN = 64
DEVIATION_FIELDS = {
    "id",
    "cell",
    "behavior_difference",
    "safety_risk",
    "threat_model",
    "invariant",
    "brew_anchor",
    "zapbrew_anchor",
    "reproduction",
    "alternatives_rejected",
    "evidence_refs",
    "proof_digest",
    "created_at",
}
RESULT_FIELDS = {
    "cell",
    "achieved_evidence",
    "failure_evidence",
    "perf_refs",
    "deviation",
    "finding",
}
EVIDENCE_FIELDS = {"id", "kind", "platform", "anchor", "digest"}
FAILURE_FIELDS = {"status", "evidence_refs"}
WORKLOAD_FIELDS = {
    "id",
    "fixture_digest",
    "source_digest",
    "platform",
    "arch",
    "filesystem_stage",
    "filesystem_stage_digest",
    "toolchain_digest",
    "build_digest",
    "warmups",
    "samples",
    "install_loop_samples",
    "sample_artifact_digest",
}
PERF_FIELDS = {
    "id",
    "workload",
    "samples",
    "floor",
    "experiment",
    "input_scaling",
    "cell_refs",
    "classification",
}
FLOOR_FIELDS = {"expression", "variables", "dimension"}
EXPERIMENT_FIELDS = {"pre_samples", "post_samples", "action"}
SCALING_FIELDS = {"input_size", "fixture_digest", "samples"}
CLASSIFICATION_FIXED_FIELDS = {
    "grade",
    "basis",
    "baseline_workload",
    "probe_workload",
    "probe_source_digest",
    "signed_median_delta_seconds",
    "baseline_over_probe_ratio",
    "review_evidence",
}
CLASSIFICATION_ATFLOOR_FIELDS = {"grade"}
CLASSIFICATION_HOT_FIELDS = {"grade"}
CLASSIFICATION_GRADES = {"fixed", "at-floor", "hot"}
FIXED_BASES = {"stage-disabled", "unit-attribution"}
REVIEW_EVIDENCE_FIELDS = {"anchor", "digest"}
APPROVAL_FIELDS = {
    "event_id",
    "kind",
    "subject_id",
    "subject_digest",
    "homebrew_commit",
    "repository",
    "actor",
    "object_url",
    "object_body_digest",
    "event_time",
}
TRANCHE_REQUIRED_FIELDS = {"event_id", "tranche_id", "from", "to", "event_time"}
TRANCHE_OPTIONAL_FIELDS = {
    "cells",
    "review_ref",
    "decision_refs",
    "reason",
    "repository_commit",
    "workload_refs",
}


@dataclass
class Report:
    integrity: list[str] = field(default_factory=list)
    incomplete: list[str] = field(default_factory=list)
    notes: list[str] = field(default_factory=list)

    @property
    def code(self) -> int:
        if self.integrity:
            return 2
        if self.incomplete:
            return 1
        return 0

    def render(self) -> str:
        if self.code == 2:
            heading = "INVALID: ledger integrity failed (exit 2)"
            items = self.integrity
        elif self.code == 1:
            heading = "VALID / INCOMPLETE: ledger is structurally valid (exit 1)"
            items = self.incomplete
        else:
            heading = "COMPLETE: strict stabilization predicate satisfied (exit 0)"
            items = []
        lines = [heading]
        lines.extend(f"  - {item}" for item in items)
        lines.extend(f"  * {item}" for item in self.notes)
        return "\n".join(lines)


def canonical_bytes(value: Any) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()


def digest(value: Any) -> str:
    return hashlib.sha256(canonical_bytes(value)).hexdigest()


def text_digest(value: str) -> str:
    return hashlib.sha256(value.encode()).hexdigest()


def valid_sha256(value: Any) -> bool:
    return (
        isinstance(value, str)
        and len(value) == SHA256_LEN
        and all(c in "0123456789abcdef" for c in value.lower())
    )


def valid_git_object_id(value: Any) -> bool:
    return (
        isinstance(value, str)
        and len(value) in {40, 64}
        and all(c in "0123456789abcdef" for c in value)
    )


def validate_references(
    value: Any, field: str, known: dict[str, Any], label: str, report: Report
) -> bool:
    if (
        not isinstance(value, list)
        or not value
        or any(not isinstance(ref, str) or not ref.strip() for ref in value)
    ):
        report.integrity.append(
            f"{label}.{field} must be a non-empty array of unique non-empty strings"
        )
        return False
    if len(set(value)) != len(value):
        report.integrity.append(f"{label}.{field} must contain unique references")
        return False
    unknown = sorted(ref for ref in value if ref not in known)
    if unknown:
        report.integrity.append(f"{label}.{field} references unknown IDs {unknown}")
        return False
    return True


def unique_map(rows: Any, key: str, label: str, report: Report) -> dict[str, dict[str, Any]]:
    result: dict[str, dict[str, Any]] = {}
    if not isinstance(rows, list):
        report.integrity.append(f"{label} must be an array")
        return result
    for index, row in enumerate(rows):
        if not isinstance(row, dict):
            report.integrity.append(f"{label}[{index}] must be an object")
            continue
        value = row.get(key)
        if not isinstance(value, str) or not value:
            report.integrity.append(f"{label}[{index}].{key} must be a non-empty string")
            continue
        if value in result:
            report.integrity.append(f"{label} has duplicate {key} {value}")
            continue
        result[value] = row
    return result


def parse_time(value: Any, label: str, report: Report) -> datetime | None:
    if not isinstance(value, str):
        report.integrity.append(f"{label} must be an RFC3339 timestamp")
        return None
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        report.integrity.append(f"{label} must be an RFC3339 timestamp")
        return None
    if parsed.tzinfo is None:
        report.integrity.append(f"{label} must include a timezone")
        return None
    return parsed


def parse_time_soft(value: Any) -> datetime | None:
    """Parse a fetched RFC3339 timestamp without recording an integrity error.

    Fetched object data is dynamic: an unparseable creation time is an
    unresolved authority-proof problem, never a permanent ledger integrity brick.
    """
    if not isinstance(value, str):
        return None
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    if parsed.tzinfo is None:
        return None
    return parsed


def load_json(path: Path, label: str, report: Report) -> Any:
    try:
        return json.loads(path.read_text())
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        report.integrity.append(f"cannot parse {label}: {exc}")
        return None


def load_jsonl(path: Path, label: str, report: Report) -> list[Any]:
    try:
        text = path.read_text()
    except (OSError, UnicodeError) as exc:
        report.integrity.append(f"cannot read {label}: {exc}")
        return []
    rows: list[Any] = []
    for number, line in enumerate(text.splitlines(), 1):
        if not line.strip():
            report.integrity.append(f"{label}:{number}: blank JSONL event")
            continue
        try:
            rows.append(json.loads(line))
        except json.JSONDecodeError as exc:
            report.integrity.append(f"{label}:{number}: malformed JSON: {exc}")
    return rows


def expected_enums() -> dict[str, list[Any]]:
    return {
        "platforms": list(PLATFORMS),
        "effects": list(EFFECTS),
        "dispositions": list(DISPOSITIONS),
        "evidence_kinds": list(EVIDENCE_KINDS),
        "finding_tags": list(FINDINGS),
        "completed_dispositions": list(COMPLETED_DISPOSITIONS),
        "approval_kinds": list(APPROVAL_KINDS),
        "tranche_states": list(TRANCHE_STATES),
        "tranche_edges": [list(edge) for edge in TRANCHE_EDGES],
        "mutation_effects": list(MUTATION_EFFECTS),
        "native_effects": list(NATIVE_EFFECTS),
        "native_proof_kinds": list(NATIVE_PROOF_KINDS),
    }


def validate_scope(
    scope: Any,
    ledger_root: Path | None,
    report: Report,
) -> tuple[dict[str, Any], dict[str, Any], dict[str, Any]]:
    if not isinstance(scope, dict):
        report.integrity.append("scope.json must contain an object")
        return {}, {}, {}
    fixed = {
        "schema_version": SCHEMA_VERSION,
        "homebrew_target": HOMEBREW_TARGET,
        "homebrew_version": "6.0.16",
        "homebrew_commit": HOMEBREW_COMMIT,
        "precedence": PRECEDENCE,
        "authority": AUTHORITY,
        "repository": REPOSITORY,
    }
    for key, expected in fixed.items():
        if scope.get(key) != expected:
            report.integrity.append(f"scope.{key} must equal {expected!r}")
    scope_enums = scope.get("enums")
    expected = expected_enums()
    if not isinstance(scope_enums, dict) or set(scope_enums) != set(expected):
        report.integrity.append("scope.enums has non-canonical closed-set names")
    else:
        for key, expected_list in expected.items():
            actual = scope_enums.get(key)
            if key == "tranche_edges":
                actual_set = {tuple(edge) for edge in actual} if isinstance(actual, list) else set()
                expected_set = {tuple(edge) for edge in expected_list}
                if actual_set != expected_set:
                    report.integrity.append(f"scope.enums.{key} does not match the canonical closed set")
            elif set(actual) != set(expected_list):
                report.integrity.append(f"scope.enums.{key} does not match the canonical closed set")


    file_map = unique_map(scope.get("files"), "path", "scope.files", report)
    modules: dict[str, str] = {}
    for path, row in file_map.items():
        if set(row) != {"path", "module"}:
            report.integrity.append(f"file row {path} has non-canonical fields")
        module = row.get("module")
        if not isinstance(module, str) or not module:
            report.integrity.append(f"file row {path} has invalid module")
        elif module in modules:
            report.integrity.append(f"duplicate scope module {module}: {modules[module]} and {path}")
        else:
            modules[module] = path
        if not (path.startswith("crates/") and "/src/" in path and path.endswith(".rs")):
            report.integrity.append(f"file row is outside production Rust universe: {path}")
    if list(file_map) != sorted(file_map):
        report.integrity.append("scope.files must be sorted by path")

    if ledger_root is not None:
        repo_root = ledger_root.parent.parent
        disk_files = {
            path.relative_to(repo_root).as_posix()
            for path in (repo_root / "crates").glob("*/src/**/*.rs")
            if path.is_file()
        }
        scoped_files = set(file_map)
        if len(disk_files) != CANONICAL_FILE_COUNT:
            report.integrity.append(
                f"disk production Rust universe must contain {CANONICAL_FILE_COUNT} files, found {len(disk_files)}"
            )
        missing = sorted(disk_files - scoped_files)
        extra = sorted(scoped_files - disk_files)
        if missing or extra:
            report.integrity.append(
                "scope file universe mismatch: "
                f"missing={missing or 'none'} extra={extra or 'none'}"
            )

    decisions = unique_map(scope.get("decisions"), "id", "scope.decisions", report)
    if set(decisions) != DECISION_IDS:
        report.integrity.append(
            f"scope decisions must be exactly D2-D8; missing={sorted(DECISION_IDS-set(decisions))} "
            f"extra={sorted(set(decisions)-DECISION_IDS)}"
        )
    if list(decisions) != sorted(decisions):
        report.integrity.append("scope.decisions must be sorted by id")
    for decision_id, decision in decisions.items():
        allowed = {"id", "status", "title", "question", "default_policy", "resolution", "decided_at"}
        if not set(decision) <= allowed:
            report.integrity.append(f"decision {decision_id} has unknown fields")
        status = decision.get("status")
        if status not in {"pending", "resolved"}:
            report.integrity.append(f"decision {decision_id} has invalid status")
        for key in ("title", "question", "default_policy"):
            if not isinstance(decision.get(key), str) or not decision[key]:
                report.integrity.append(f"decision {decision_id}.{key} must be non-empty")
        if status == "pending":
            if "resolution" in decision or "decided_at" in decision:
                report.integrity.append(f"pending decision {decision_id} cannot carry a resolution")
            report.incomplete.append(f"decision pending: {decision_id}")
        elif status == "resolved":
            if not isinstance(decision.get("resolution"), str) or not decision["resolution"]:
                report.integrity.append(f"resolved decision {decision_id} needs a resolution")
            parse_time(decision.get("decided_at"), f"decision {decision_id}.decided_at", report)

    cells = unique_map(scope.get("cells"), "cell_id", "scope.cells", report)
    if len(cells) != CANONICAL_CELL_COUNT:
        report.integrity.append(
            f"scope must contain exactly {CANONICAL_CELL_COUNT} canonical cells, found {len(cells)}"
        )
    if list(cells) != sorted(cells):
        report.integrity.append("scope.cells must be sorted by cell_id")
    canonical_cell_fields = {
        "cell_id",
        "module",
        "behavior",
        "platform",
        "effect",
        "expected_disposition",
        "expected_evidence",
        "decision_refs",
        "source_pair",
    }
    for cell_id, cell in cells.items():
        if set(cell) != canonical_cell_fields:
            report.integrity.append(f"cell {cell_id} has non-canonical fields")
        module = cell.get("module")
        if module not in modules:
            report.integrity.append(f"cell {cell_id} references unknown module {module!r}")
        behavior = cell.get("behavior")
        if not isinstance(behavior, str) or not behavior.strip():
            report.integrity.append(f"cell {cell_id} has empty behavior")
        platform = cell.get("platform")
        effect = cell.get("effect")
        disposition = cell.get("expected_disposition")
        evidence = cell.get("expected_evidence")
        refs = cell.get("decision_refs")
        if platform not in PLATFORMS:
            report.integrity.append(f"cell {cell_id} has invalid platform {platform!r}")
        if effect not in EFFECTS:
            report.integrity.append(f"cell {cell_id} has invalid effect {effect!r}")
        if disposition not in DISPOSITIONS:
            report.integrity.append(f"cell {cell_id} has invalid disposition {disposition!r}")
        if not isinstance(evidence, list) or not evidence:
            report.integrity.append(f"cell {cell_id} expected_evidence must be non-empty")
            evidence_set: set[str] = set()
        else:
            evidence_set = set(evidence)
            if len(evidence_set) != len(evidence):
                report.integrity.append(f"cell {cell_id} repeats an evidence kind")
            unknown = evidence_set - EVIDENCE_KINDS
            if unknown:
                report.integrity.append(f"cell {cell_id} has unknown evidence kinds {sorted(unknown)}")
        if not isinstance(refs, list) or any(not isinstance(ref, str) for ref in refs):
            report.integrity.append(f"cell {cell_id}.decision_refs must be string array")
            refs = []
        elif len(set(refs)) != len(refs):
            report.integrity.append(f"cell {cell_id} repeats a decision reference")
        for ref in refs:
            if ref not in decisions:
                report.integrity.append(f"cell {cell_id} references unknown decision {ref}")

        if disposition == "product-decision-pending" and not refs:
            report.integrity.append(
                f"cell {cell_id} is product-decision-pending with no controlling decision reference"
            )
        elif disposition == "product-decision-pending" and refs and all(
            decisions.get(ref, {}).get("status") == "resolved" for ref in refs
        ):
            report.integrity.append(
                f"cell {cell_id} remains product-decision-pending after all controlling decisions resolved"
            )
        validate_source_pair(cell_id, cell, file_map, report)
        validate_evidence_matrix(cell_id, platform, effect, evidence_set, report)
    return cells, decisions, file_map


def validate_source_pair(
    cell_id: str, cell: dict[str, Any], file_map: dict[str, Any], report: Report
) -> None:
    pair = cell.get("source_pair")
    if not isinstance(pair, dict) or set(pair) != {"zapbrew", "homebrew"}:
        report.integrity.append(f"cell {cell_id} source_pair must contain exactly zapbrew/homebrew")
        return
    zapbrew = pair.get("zapbrew")
    homebrew = pair.get("homebrew")
    if not isinstance(zapbrew, str) or not zapbrew:
        report.integrity.append(f"cell {cell_id} has invalid Zapbrew source anchor")
    else:
        source_path = zapbrew.split(":", 1)[0]
        if source_path not in file_map:
            report.integrity.append(f"cell {cell_id} Zapbrew anchor is outside scoped files: {source_path}")
    disposition = cell.get("expected_disposition")
    if not isinstance(homebrew, str) or not homebrew.strip():
        report.integrity.append(f"cell {cell_id} has invalid Homebrew source anchor")
    elif homebrew == "pending/unresolved" or homebrew.lower().startswith("unresolved"):
        report.integrity.append(f"cell {cell_id} has unresolved Homebrew source anchor")
    elif disposition in {"must-match", "safety-deviation-candidate"} and not homebrew.startswith(HOMEBREW_ANCHOR_PREFIXES):
        report.integrity.append(
            f"cell {cell_id} {disposition} anchor is not pinned to Homebrew 6.0.16 or {HOMEBREW_COMMIT}"
        )


def validate_evidence_matrix(
    cell_id: str, platform: Any, effect: Any, evidence: set[str], report: Report
) -> None:
    def require(kinds: set[str], reason: str) -> None:
        missing = kinds - evidence
        if missing:
            report.integrity.append(f"cell {cell_id} lacks {sorted(missing)} for {reason}")

    if effect == "filesystem-mutation":
        require({"unit-fixture-io"}, "filesystem mutation")
        if not evidence.intersection(NATIVE_PROOF_KINDS):
            report.integrity.append(f"cell {cell_id} lacks execution proof for filesystem mutation")
        if platform == "macos":
            require({"cross-compile"}, "macOS filesystem mutation")
    if effect in NATIVE_EFFECTS:
        require({"command-construction"}, "native effect")
        if platform in {"linux", "macos"} and not evidence.intersection(NATIVE_PROOF_KINDS):
            report.integrity.append(f"cell {cell_id} lacks native execution proof")
    if effect == "native-query" and "unit-fixture-io" in evidence and "command-construction" not in evidence:
        report.integrity.append(f"native-query cell {cell_id} cannot use fixture I/O as native proof")
    if "native-side-effect" in evidence and effect not in MUTATION_EFFECTS | NATIVE_EFFECTS:
        report.integrity.append(f"cell {cell_id} requests native-side-effect for non-native effect")
    if platform == "any" and effect in MUTATION_EFFECTS:
        require({"cross-compile"}, "platform-any mutation")


def validate_results(
    data: Any,
    cells: dict[str, Any],
    report: Report,
) -> tuple[dict[str, Any], dict[str, Any], dict[str, Any], dict[str, Any], dict[str, set[str]], dict[str, dict[str, Any]]]:
    if not isinstance(data, dict):
        report.integrity.append("results.json must contain an object")
        return {}, {}, {}, {}, {}, {}
    if set(data) != {"results", "deviations", "workloads", "perf_units"}:
        report.integrity.append("results.json has non-canonical top-level fields")

    results = unique_map(data.get("results"), "cell", "results.results", report)
    if list(results) != sorted(results):
        report.integrity.append("results.results must be sorted by cell")
    missing = sorted(set(cells) - set(results))
    orphan = sorted(set(results) - set(cells))
    if missing or orphan:
        report.integrity.append(
            f"cell/result bijection failed: missing={missing or 'none'} orphan={orphan or 'none'}"
        )

    evidence_ids: dict[str, set[str]] = {}
    for cell_id, result in results.items():
        if set(result) != RESULT_FIELDS:
            report.integrity.append(f"result {cell_id} has non-canonical fields")
        finding = result.get("finding")
        if finding not in FINDINGS:
            report.integrity.append(f"result {cell_id} has invalid finding {finding!r}")
        achieved = result.get("achieved_evidence")
        ids: set[str] = set()
        if not isinstance(achieved, list):
            report.integrity.append(f"result {cell_id}.achieved_evidence must be an array")
            achieved = []
        for index, item in enumerate(achieved):
            label = f"result {cell_id}.achieved_evidence[{index}]"
            if not isinstance(item, dict) or set(item) != EVIDENCE_FIELDS:
                report.integrity.append(f"{label} has non-canonical fields")
                continue
            evidence_id = item.get("id")
            if not isinstance(evidence_id, str) or not evidence_id:
                report.integrity.append(f"{label}.id must be non-empty")
            elif evidence_id in ids:
                report.integrity.append(f"result {cell_id} repeats evidence id {evidence_id}")
            else:
                ids.add(evidence_id)
            if item.get("kind") not in EVIDENCE_KINDS:
                report.integrity.append(f"{label} has invalid evidence kind")
            if item.get("platform") not in PLATFORMS:
                report.integrity.append(f"{label} has invalid platform")
            if not isinstance(item.get("anchor"), str) or not item["anchor"]:
                report.integrity.append(f"{label} has invalid anchor")
            if not valid_sha256(item.get("digest")):
                report.integrity.append(f"{label} has invalid digest")

            cell = cells.get(cell_id, {})
            if (
                item.get("kind") in NATIVE_PROOF_KINDS
                and cell.get("platform") in {"linux", "macos"}
                and item.get("platform") != cell.get("platform")
            ):
                report.integrity.append(
                    f"{label} kind={item['kind']}! native proof platform {item.get('platform')} "
                    f"does not match cell {cell_id} platform {cell.get('platform')}"
                )

        evidence_ids[cell_id] = ids

        failure = result.get("failure_evidence")
        if failure is not None:
            if not isinstance(failure, dict) or set(failure) != FAILURE_FIELDS:
                report.integrity.append(f"result {cell_id}.failure_evidence has non-canonical fields")
            else:
                if failure.get("status") not in {"proved", "defect", "unresolved"}:
                    report.integrity.append(f"result {cell_id}.failure_evidence has invalid status")
                refs = failure.get("evidence_refs")
                if not isinstance(refs, list) or any(not isinstance(ref, str) for ref in refs):
                    report.integrity.append(f"result {cell_id}.failure_evidence refs must be string array")
                elif any(ref not in ids for ref in refs):
                    report.integrity.append(f"result {cell_id}.failure_evidence has bad evidence ref")
                elif failure.get("status") == "proved" and not refs:
                    report.integrity.append(f"result {cell_id}.proved failure evidence needs refs")
        if cells.get(cell_id, {}).get("effect") == "native-query" and failure is not None:
            report.integrity.append(f"native-query cell {cell_id} cannot carry mutation failure evidence")

        perf_refs = result.get("perf_refs")
        if not isinstance(perf_refs, list) or any(not isinstance(ref, str) for ref in perf_refs):
            report.integrity.append(f"result {cell_id}.perf_refs must be a string array")
        elif len(set(perf_refs)) != len(perf_refs):
            report.integrity.append(f"result {cell_id} repeats a performance ref")
        deviation = result.get("deviation")
        if deviation is not None and (not isinstance(deviation, str) or not deviation):
            report.integrity.append(f"result {cell_id}.deviation must be null or a non-empty id")

    deviations = validate_deviations(data.get("deviations"), cells, results, evidence_ids, report)
    workloads = validate_workloads(data.get("workloads"), report)
    perf_units, hot_markers = validate_perf_units(data.get("perf_units"), workloads, cells, report)

    for cell_id, result in results.items():
        for ref in result.get("perf_refs", []) if isinstance(result.get("perf_refs"), list) else []:
            if ref not in perf_units:
                report.integrity.append(f"result {cell_id} references unknown performance unit {ref}")
        deviation_ref = result.get("deviation")
        if deviation_ref is not None and deviation_ref not in deviations:
            report.integrity.append(f"result {cell_id} references unknown deviation {deviation_ref}")
    return results, deviations, workloads, perf_units, evidence_ids, hot_markers


def validate_deviations(
    rows: Any,
    cells: dict[str, Any],
    results: dict[str, Any],
    evidence_ids: dict[str, set[str]],
    report: Report,
) -> dict[str, Any]:
    deviations = unique_map(rows, "id", "results.deviations", report)
    for deviation_id, deviation in deviations.items():
        if set(deviation) != DEVIATION_FIELDS:
            report.integrity.append(f"deviation {deviation_id} has non-canonical fields")
            continue
        cell_id = deviation.get("cell")
        cell = cells.get(cell_id, {})
        if cell.get("expected_disposition") != "safety-deviation-candidate":
            report.integrity.append(
                f"deviation {deviation_id} is not for a safety-deviation-candidate cell"
            )
        for key in ("behavior_difference", "safety_risk", "threat_model", "invariant"):
            if not isinstance(deviation.get(key), str) or not deviation[key]:
                report.integrity.append(f"deviation {deviation_id}.{key} must be non-empty")
        for key in ("brew_anchor", "zapbrew_anchor", "reproduction", "alternatives_rejected"):
            if not isinstance(deviation.get(key), str) or not deviation[key]:
                report.integrity.append(f"deviation {deviation_id}.{key} must be non-empty")
        proof_digest = deviation.get("proof_digest")
        if not valid_sha256(proof_digest):
            report.integrity.append(f"deviation {deviation_id}.proof_digest must be sha256")
        created_at = deviation.get("created_at")
        parse_time(created_at, f"deviation {deviation_id}.created_at", report)
        refs = deviation.get("evidence_refs")
        if not isinstance(refs, list) or any(not isinstance(ref, str) for ref in refs):
            report.integrity.append(f"deviation {deviation_id}.evidence_refs must be a string array")
        else:
            known = evidence_ids.get(cell_id, set())
            if any(ref not in known for ref in refs):
                report.integrity.append(f"deviation {deviation_id} has bad evidence ref")
        result = results.get(cell_id, {})
        if result and result.get("deviation") != deviation_id:
            report.integrity.append(
                f"deviation {deviation_id} is not referenced by result {cell_id}"
            )
        if result:
            achieved_kinds = {
                item.get("kind") for item in result.get("achieved_evidence", []) if isinstance(item, dict)
            }
            required = set(cell.get("expected_evidence", []))
            if missing := sorted(required - achieved_kinds):
                report.integrity.append(
                    f"deviation {deviation_id} cell lacks required evidence kinds {missing}"
                )
        if result and cell.get("expected_evidence"):
            cell_digest = digest(
                {
                    "cell_id": cell_id,
                    "behavior": cell.get("behavior"),
                    "platform": cell.get("platform"),
                    "effect": cell.get("effect"),
                    "expected_disposition": cell.get("expected_disposition"),
                    "expected_evidence": cell.get("expected_evidence"),
                    "decision_refs": cell.get("decision_refs"),
                    "source_pair": cell.get("source_pair"),
                    "achieved_evidence": result.get("achieved_evidence"),
                    "failure_evidence": result.get("failure_evidence"),
                    "deviation": result.get("deviation"),
                }
            )
            if proof_digest != text_digest(cell_digest):
                report.integrity.append(f"deviation {deviation_id} stale proof digest")
    return deviations


def numeric_samples(value: Any, label: str, report: Report) -> list[float]:
    if not isinstance(value, list) or any(
        isinstance(item, bool)
        or not isinstance(item, (int, float))
        or not math.isfinite(float(item))
        or float(item) <= 0
        for item in value
    ):
        report.integrity.append(f"{label} must be a list of positive finite numbers")
        return []
    return [float(sample) for sample in value]


def trusted_samples(samples: list[float], label: str, report: Report, minimum_median: float | None = None) -> bool:
    trusted = True
    if len(samples) < 10:
        report.integrity.append(f"{label} is undersized (need >=10, got {len(samples)})")
        trusted = False
    if samples:
        median = statistics.median(samples)
        if minimum_median is not None and median < minimum_median:
            report.integrity.append(f"{label} median {median} is below minimum {minimum_median}")
            trusted = False
        if len(samples) >= 2 and not statistics.stdev(samples) < 0.20 * statistics.median(samples):
            stdev = statistics.stdev(samples)
            report.integrity.append(
                f"{label} is noisy (stdev {stdev:.6g} >= 20% of median {median:.6g})"
            )
            trusted = False
    return trusted


def validate_workloads(rows: Any, report: Report) -> dict[str, Any]:
    workloads = unique_map(rows, "id", "results.workloads", report)
    for workload_id, workload in workloads.items():
        if set(workload) != WORKLOAD_FIELDS:
            report.integrity.append(f"workload {workload_id} has non-canonical fields")
            continue
        for key in (
            "fixture_digest",
            "source_digest",
            "filesystem_stage_digest",
            "toolchain_digest",
            "build_digest",
            "sample_artifact_digest",
        ):
            if not valid_sha256(workload.get(key)):
                report.integrity.append(f"workload {workload_id}.{key} must be sha256")
        if workload.get("platform") not in {"linux", "macos"}:
            report.integrity.append(f"workload {workload_id}.platform must be linux or macos")
        for key in ("arch", "filesystem_stage"):
            if not isinstance(workload.get(key), str) or not workload[key]:
                report.integrity.append(f"workload {workload_id}.{key} must be non-empty")
        warmups = workload.get("warmups")
        if not isinstance(warmups, int) or isinstance(warmups, bool) or warmups < 3:
            report.integrity.append(f"workload {workload_id}.warmups must be >=3")
        samples = numeric_samples(workload.get("samples"), f"workload {workload_id}.samples", report)
        trusted_samples(samples, f"workload {workload_id}.samples", report, minimum_median=1.0)
        loop = numeric_samples(
            workload.get("install_loop_samples"),
            f"workload {workload_id}.install_loop_samples",
            report,
        )
        trusted_samples(loop, f"workload {workload_id}.install_loop_samples", report)
        if len(loop) != len(samples):
            report.integrity.append(
                f"workload {workload_id}.install_loop_samples not aligned to wall samples"
            )
        if samples and loop and statistics.median(loop) < 0.90 * statistics.median(samples):
            report.integrity.append(f"workload {workload_id} install loop below 90% of wall time")
    return workloads


def validate_floor_expression(expression: Any, variables: Any, label: str, report: Report) -> float | None:
    if not isinstance(expression, str) or not expression:
        report.integrity.append(f"{label}.expression must be non-empty")
        return None
    if not isinstance(variables, dict) or any(
        not isinstance(name, str)
        or not name.isidentifier()
        or isinstance(value, bool)
        or not isinstance(value, (int, float))
        or not math.isfinite(float(value))
        or value <= 0
        for name, value in variables.items()
    ):
        report.integrity.append(f"{label}.variables must map identifiers to positive finite numbers")
        return None
    try:
        tree = ast.parse(expression, mode="eval")
    except SyntaxError:
        report.integrity.append(f"{label}.expression is not valid arithmetic")
        return None

    def evaluate(node: ast.AST) -> float:
        if isinstance(node, ast.Expression):
            return evaluate(node.body)
        if isinstance(node, ast.Constant):
            value = node.value
            if isinstance(value, (int, float)) and not isinstance(value, bool):
                return float(value)
            raise ValueError(type(value).__name__)
        if isinstance(node, ast.Name) and node.id in variables:
            value = variables[node.id]
            if isinstance(value, (int, float)) and not isinstance(value, bool):
                return float(value)
            raise ValueError(type(value).__name__)
        if isinstance(node, ast.BinOp):
            left = evaluate(node.left)
            right = evaluate(node.right)
            if isinstance(node.op, ast.Add):
                return left + right
            if isinstance(node.op, ast.Sub):
                return left - right
            if isinstance(node.op, ast.Mult):
                return left * right
            if isinstance(node.op, ast.Div):
                return left / right
            raise ValueError(type(node.op).__name__)
        if isinstance(node, ast.UnaryOp):
            value = evaluate(node.operand)
            if isinstance(node.op, ast.UAdd):
                return value
            if isinstance(node.op, ast.USub):
                return -value
            raise ValueError(type(node.op).__name__)
        raise ValueError(type(node).__name__)

    try:
        result = evaluate(tree)
    except (ValueError, ZeroDivisionError, OverflowError):
        report.integrity.append(f"{label}.expression contains unsafe or invalid syntax")
        return None
    if not math.isfinite(result) or result <= 0:
        report.integrity.append(f"{label}.expression must evaluate to a positive finite floor")
        return None
    return result


def validate_perf_units(
    rows: Any, workloads: dict[str, Any], cells: dict[str, Any], report: Report
) -> tuple[dict[str, Any], dict[str, dict[str, Any]]]:
    units = unique_map(rows, "id", "results.perf_units", report)
    hot_markers: dict[str, dict[str, Any]] = {}
    for unit_id, unit in units.items():
        if set(unit) != PERF_FIELDS:
            report.integrity.append(f"performance unit {unit_id} has non-canonical fields")
            continue
        workload_id = unit.get("workload")
        workload = workloads.get(workload_id)
        if workload is None:
            report.integrity.append(f"performance unit {unit_id} references unknown workload {workload_id}")
        cell_refs = unit.get("cell_refs")
        if not isinstance(cell_refs, list) or not cell_refs:
            report.integrity.append(f"performance unit {unit_id}.cell_refs must be non-empty")
        elif len(set(cell_refs)) != len(cell_refs) or any(ref not in cells for ref in cell_refs):
            report.integrity.append(f"performance unit {unit_id} has duplicate or unknown cell refs")
        else:
            workload_platform = workload.get("platform") if workload is not None else None
            for ref in cell_refs:
                cell_platform = cells[ref].get("platform")
                if workload_platform is not None and cell_platform not in {"any", workload_platform}:
                    report.integrity.append(
                        f"performance unit {unit_id} workload platform {workload_platform} "
                        f"cannot attribute to {cell_platform} cell {ref}"
                    )

        classification = unit.get("classification")
        if not isinstance(classification, dict):
            report.integrity.append(f"performance unit {unit_id}.classification must be an object")
            continue
        grade = classification.get("grade")
        expected_classification_fields = {
            "fixed": CLASSIFICATION_FIXED_FIELDS,
            "at-floor": CLASSIFICATION_ATFLOOR_FIELDS,
            "hot": CLASSIFICATION_HOT_FIELDS,
        }.get(grade)
        if grade not in CLASSIFICATION_GRADES:
            report.integrity.append(f"performance unit {unit_id} has invalid classification grade")
            continue
        if set(classification) != expected_classification_fields:
            report.integrity.append(
                f"performance unit {unit_id}.classification has non-canonical fields for {grade}"
            )
            continue

        scaling = unit.get("input_scaling")
        scaling_hot = False
        if not isinstance(scaling, list):
            report.integrity.append(f"performance unit {unit_id}.input_scaling must be an array")
            scaling = []
        if scaling:
            if len(scaling) < 2:
                report.integrity.append(f"performance unit {unit_id} scaling needs at least two sizes")
            sizes: list[tuple[float, float]] = []
            seen_sizes: set[float] = set()
            for index, row in enumerate(scaling):
                label = f"performance unit {unit_id}.input_scaling[{index}]"
                if not isinstance(row, dict) or set(row) != SCALING_FIELDS:
                    report.integrity.append(f"{label} has non-canonical fields")
                    continue
                size = row.get("input_size")
                if isinstance(size, bool) or not isinstance(size, (int, float)) or size <= 0:
                    report.integrity.append(f"{label}.input_size must be positive")
                    continue
                if float(size) in seen_sizes:
                    report.integrity.append(f"performance unit {unit_id} repeats an input size")
                seen_sizes.add(float(size))
                if not valid_sha256(row.get("fixture_digest")):
                    report.integrity.append(f"{label}.fixture_digest must be sha256")
                raw = numeric_samples(row.get("samples"), f"{label}.samples", report)
                trusted_samples(raw, f"{label}.samples", report)
                if raw:
                    sizes.append((float(size), statistics.median(raw)))
            sizes.sort()
            if len(sizes) >= 2 and sizes[-1][0] > sizes[0][0] and sizes[-1][1] > sizes[0][1] * 1.05:
                scaling_hot = True

        if grade == "fixed":
            basis = classification.get("basis")
            if basis not in FIXED_BASES:
                report.integrity.append(f"performance unit {unit_id} has invalid fixed basis")
            if unit.get("floor") is not None:
                report.integrity.append(f"fixed unit {unit_id} carries a floor")
            if unit.get("experiment") is not None:
                report.integrity.append(f"fixed unit {unit_id} carries an optimization experiment")
            if scaling:
                report.integrity.append(f"fixed unit {unit_id} carries a scaling claim")

            direct_samples = numeric_samples(
                unit.get("samples"), f"performance unit {unit_id}.samples", report
            )
            if basis == "stage-disabled" and direct_samples:
                report.integrity.append(f"fixed unit {unit_id} carries direct unit samples")
            if basis == "unit-attribution":
                trusted_samples(direct_samples, f"performance unit {unit_id}.samples", report)
                if not direct_samples:
                    report.integrity.append(f"fixed unit {unit_id} lacks direct unit samples")
                if (
                    workload is not None
                    and isinstance(workload.get("samples"), list)
                    and len(direct_samples) != len(workload["samples"])
                ):
                    report.integrity.append(
                        f"performance unit {unit_id} samples are not aligned to workload runs"
                    )
                if direct_samples and workload is not None and workload.get("samples"):
                    baseline_median = statistics.median(float(x) for x in workload["samples"])
                    if statistics.median(direct_samples) / baseline_median >= 0.05:
                        report.integrity.append(
                            f"fixed grade rejected: direct attribution is >=5% of wall time"
                        )

            baseline_id = classification.get("baseline_workload")
            probe_id = classification.get("probe_workload")
            baseline = workloads.get(baseline_id)
            probe = workloads.get(probe_id)
            if baseline is None:
                report.integrity.append(
                    f"fixed unit {unit_id} references unknown baseline workload {baseline_id}"
                )
            if probe is None:
                report.integrity.append(f"fixed unit {unit_id} references unknown probe workload {probe_id}")
            if baseline_id != workload_id:
                report.integrity.append(f"fixed baseline workload must equal unit workload for {unit_id}")
            if baseline_id == probe_id:
                report.integrity.append(f"fixed probe must differ from baseline for {unit_id}")

            probe_source_digest = classification.get("probe_source_digest")
            if not valid_sha256(probe_source_digest):
                report.integrity.append(f"fixed unit {unit_id}.probe_source_digest must be sha256")
            if baseline is not None and probe_source_digest == baseline.get("build_digest"):
                report.integrity.append(f"fixed unit {unit_id} overloads baseline build digest as probe source")
            review_evidence = classification.get("review_evidence")
            if not isinstance(review_evidence, dict) or set(review_evidence) != REVIEW_EVIDENCE_FIELDS:
                report.integrity.append(f"fixed unit {unit_id}.review_evidence has non-canonical fields")
            else:
                if not isinstance(review_evidence.get("anchor"), str) or not review_evidence["anchor"]:
                    report.integrity.append(f"fixed unit {unit_id}.review_evidence.anchor must be non-empty")
                if not valid_sha256(review_evidence.get("digest")):
                    report.integrity.append(f"fixed unit {unit_id}.review_evidence.digest must be sha256")

            if baseline is not None and probe is not None:
                if basis == "stage-disabled":
                    for key in (
                        "platform",
                        "arch",
                        "fixture_digest",
                        "source_digest",
                        "filesystem_stage",
                        "filesystem_stage_digest",
                        "toolchain_digest",
                        "warmups",
                    ):
                        if baseline.get(key) != probe.get(key):
                            report.integrity.append(
                                f"stage-disabled probe for {unit_id} is not aligned on {key}"
                            )
                    if len(baseline.get("samples", [])) != len(probe.get("samples", [])):
                        report.integrity.append(
                            f"stage-disabled probe for {unit_id} is not aligned on sample count"
                        )
                    if baseline.get("build_digest") == probe.get("build_digest"):
                        report.integrity.append(
                            "stage-disabled probe is the same build as baseline"
                        )
                baseline_samples = numeric_samples(
                    baseline.get("samples"), f"fixed unit {unit_id}.baseline samples", report
                )
                probe_samples = numeric_samples(
                    probe.get("samples"), f"fixed unit {unit_id}.probe samples", report
                )
                if baseline_samples and probe_samples:
                    baseline_median = statistics.median(baseline_samples)
                    probe_median = statistics.median(probe_samples)
                    measured_delta = baseline_median - probe_median
                    signed_delta = classification.get("signed_median_delta_seconds")
                    if (
                        isinstance(signed_delta, bool)
                        or not isinstance(signed_delta, (int, float))
                        or not math.isfinite(float(signed_delta))
                        or not math.isclose(
                            float(signed_delta), measured_delta, rel_tol=1e-9, abs_tol=1e-12
                        )
                    ):
                        report.integrity.append(
                            f"fixed signed delta does not match measured medians for {unit_id}"
                        )
                    measured_ratio = baseline_median / probe_median
                    ratio = classification.get("baseline_over_probe_ratio")
                    if (
                        isinstance(ratio, bool)
                        or not isinstance(ratio, (int, float))
                        or not math.isfinite(float(ratio))
                        or not math.isclose(float(ratio), measured_ratio, rel_tol=1e-9)
                    ):
                        report.integrity.append(f"fixed ratio mismatch for {unit_id}")
                    if basis == "stage-disabled":
                        speedup = measured_delta / baseline_median
                        if speedup >= 0.05:
                            report.integrity.append(
                                "fixed grade rejected: disabling the stage saves >=5% (hot cost)"
                            )
            continue

        samples = numeric_samples(unit.get("samples"), f"performance unit {unit_id}.samples", report)
        trusted_samples(samples, f"performance unit {unit_id}.samples", report)
        if workload is not None and isinstance(workload.get("samples"), list) and len(samples) != len(workload["samples"]):
            report.integrity.append(f"performance unit {unit_id} samples are not aligned to workload runs")

        floor = unit.get("floor")
        floor_value = None
        if not isinstance(floor, dict) or set(floor) != FLOOR_FIELDS:
            report.integrity.append(f"performance unit {unit_id}.floor has non-canonical fields")
        else:
            if not isinstance(floor.get("dimension"), str) or not floor["dimension"]:
                report.integrity.append(f"performance unit {unit_id}.floor.dimension must be non-empty")
            floor_value = validate_floor_expression(
                floor.get("expression"), floor.get("variables"), f"performance unit {unit_id}.floor", report
            )

        experiment = unit.get("experiment")
        pre: list[float] = []
        post: list[float] = []
        action = None
        if grade == "at-floor":
            if experiment is not None:
                report.integrity.append(f"at-floor unit {unit_id} carries an optimization experiment")
        elif not isinstance(experiment, dict) or set(experiment) != EXPERIMENT_FIELDS:
            report.integrity.append(f"performance unit {unit_id}.experiment has non-canonical fields")
        else:
            pre = numeric_samples(experiment.get("pre_samples"), f"performance unit {unit_id}.experiment.pre", report)
            post = numeric_samples(experiment.get("post_samples"), f"performance unit {unit_id}.experiment.post", report)
            trusted_samples(pre, f"performance unit {unit_id}.experiment.pre", report)
            trusted_samples(post, f"performance unit {unit_id}.experiment.post", report)
            action = experiment.get("action")
            if action not in {"keep", "revert"}:
                report.integrity.append(f"performance unit {unit_id}.experiment.action is invalid")
            if pre and post:
                win = statistics.median(pre) / statistics.median(post)
                if action == "keep" and win < 1.05:
                    report.integrity.append(
                        f"performance unit {unit_id} keeps a {win:.6g}x change below the 1.05x gate"
                    )

        if samples and workload is not None and workload.get("samples") and floor_value is not None:
            workload_median = statistics.median(float(x) for x in workload["samples"])
            unit_median = statistics.median(samples)
            measured_hot = unit_median / workload_median >= 0.05 or scaling_hot
            multiple = unit_median / floor_value
            if grade == "at-floor" and (not measured_hot or multiple > 2):
                report.integrity.append(
                    f"at-floor grade contradicts measurement for performance unit {unit_id}"
                )
            if grade == "hot" and not measured_hot:
                report.integrity.append(
                    f"hot grade contradicts a measured cold unit {unit_id}"
                )
            if measured_hot:
                hot_markers[unit_id] = {
                    "multiple": multiple,
                    "action": action,
                    "pre_median": statistics.median(pre) if pre else None,
                    "post_median": statistics.median(post) if post else None,
                }
    return units, hot_markers


@dataclass
class TrancheState:
    state: str
    cells: set[str] = field(default_factory=set)
    reviewed: bool = False
    landed: bool = False
    checkpoint_event_id: str | None = None
    checkpoint_time: datetime | None = None
    repository_commit: str | None = None
    review_ref: str | None = None
    approved_at: datetime | None = None
    context_decisions: set[str] = field(default_factory=set)
    context_workloads: set[str] = field(default_factory=set)


def replay_tranches(
    events: list[Any], cells: dict[str, Any], decisions: dict[str, Any], workloads: dict[str, Any], report: Report
) -> tuple[dict[str, TrancheState], dict[str, str]]:
    states: dict[str, TrancheState] = {}
    active_owner: dict[str, str] = {}
    seen_event_ids: set[str] = set()
    last_time: dict[str, datetime] = {}
    for index, event in enumerate(events):
        label = f"tranches.jsonl event {index + 1}"
        if not isinstance(event, dict):
            report.integrity.append(f"{label} must be an object")
            continue
        missing = TRANCHE_REQUIRED_FIELDS - set(event)
        if missing:
            report.integrity.append(f"{label} missing required fields {sorted(missing)}")
            continue
        unknown = set(event) - (TRANCHE_REQUIRED_FIELDS | TRANCHE_OPTIONAL_FIELDS)
        if unknown:
            report.integrity.append(f"{label} has unknown fields {sorted(unknown)}")
            continue
        event_id = event.get("event_id")
        if not isinstance(event_id, str) or not event_id:
            report.integrity.append(f"{label}.event_id must be non-empty")
            continue
        if event_id in seen_event_ids:
            report.integrity.append(f"{label} repeats tranche event_id {event_id}")
            continue
        seen_event_ids.add(event_id)
        tranche_id = event.get("tranche_id")
        if not isinstance(tranche_id, str) or not tranche_id:
            report.integrity.append(f"{label}.tranche_id must be non-empty")
            continue
        event_time = parse_time(event.get("event_time"), f"{label}.event_time", report)
        if event_time is None:
            continue
        previous_time = last_time.get(tranche_id)
        if previous_time is not None and event_time <= previous_time:
            report.integrity.append(
                f"tranche {tranche_id} event {event_id} time must be strictly increasing"
            )
            continue
        last_time[tranche_id] = event_time

        references_valid = True
        for field, known in (("decision_refs", decisions), ("workload_refs", workloads)):
            if field in event:
                references_valid &= validate_references(event.get(field), field, known, label, report)
        if "reason" in event and (
            not isinstance(event.get("reason"), str) or not event["reason"].strip()
        ):
            report.integrity.append(f"{label}.reason must be a non-empty string")
            references_valid = False
        repository_commit_present = "repository_commit" in event
        if repository_commit_present and not valid_git_object_id(event.get("repository_commit")):
            report.integrity.append(
                f"{label}.repository_commit must be one lowercase 40- or 64-character hexadecimal object ID"
            )
            references_valid = False
        if not references_valid:
            continue

        from_state = event.get("from")
        to_state = event.get("to")
        if (from_state, to_state) not in TRANCHE_EDGES:
            report.integrity.append(f"tranche {tranche_id} illegal tranche edge {from_state} -> {to_state}")
            continue
        checkpoint_edge = from_state == "evidence-pending" and to_state == "review-pending"
        if checkpoint_edge and not repository_commit_present:
            report.integrity.append(
                f"tranche {tranche_id} event {event_id} review checkpoint requires repository_commit"
            )
            continue
        if repository_commit_present and not checkpoint_edge:
            report.integrity.append(
                f"tranche {tranche_id} event {event_id}.repository_commit is only permitted on evidence-pending -> review-pending"
            )
            continue
        tranche = states.get(tranche_id)
        current_state = tranche.state if tranche is not None else "proposed"
        if current_state != from_state:
            report.integrity.append(
                f"tranche {tranche_id} event {event_id} expects state {from_state}, found {current_state}"
            )
            continue
        review_ref_present = "review_ref" in event
        review_ref = event.get("review_ref")
        if review_ref_present and (not isinstance(review_ref, str) or not review_ref.strip()):
            report.integrity.append(f"tranche {tranche_id} event {event_id}.review_ref must be a non-empty string")
            continue
        approval_edge = from_state == "review-pending" and to_state == "approved"
        if review_ref_present and not approval_edge:
            report.integrity.append(
                f"tranche {tranche_id} event {event_id}.review_ref is only permitted on review-pending -> approved"
            )
            continue
        if approval_edge and not review_ref_present:
            report.integrity.append(
                f"tranche {tranche_id} event {event_id} approval requires a non-empty review_ref"
            )
            continue
        if from_state == "proposed" and to_state == "scoped":
            if not validate_references(event.get("cells"), "cells", cells, label, report):
                continue
            cell_refs = event["cells"]
            if any(ref in active_owner for ref in cell_refs):
                report.integrity.append(
                    f"tranche {tranche_id} proposed->scoped includes cells already in another tranche"
                )
                continue
        tranche = states.setdefault(tranche_id, TrancheState(state="proposed"))
        if from_state == "proposed" and to_state == "scoped":
            for ref in cell_refs:
                active_owner[ref] = tranche_id
            tranche.cells = set(cell_refs)
            for ref in cell_refs:
                if cells.get(ref, {}).get("effect") == "native-mutation" and ref not in active_owner:
                    active_owner[ref] = tranche_id
        if checkpoint_edge:
            tranche.checkpoint_event_id = event_id
            tranche.checkpoint_time = event_time
            tranche.repository_commit = event["repository_commit"]
            tranche.review_ref = None
            tranche.approved_at = None
            tranche.reviewed = False
            tranche.context_decisions = set(event.get("decision_refs", []))
            tranche.context_workloads = set(event.get("workload_refs", []))
        if approval_edge:
            tranche.review_ref = review_ref
            tranche.approved_at = event_time
        if to_state in {"changes-requested", "reverted"}:
            tranche.checkpoint_event_id = None
            tranche.checkpoint_time = None
            tranche.repository_commit = None
            tranche.review_ref = None
            tranche.approved_at = None
            tranche.reviewed = False
            tranche.context_decisions.clear()
            tranche.context_workloads.clear()
        if to_state == "landed":
            tranche.landed = True
        elif to_state == "reverted":
            tranche.landed = False
        if to_state == "reverted":
            for ref in tranche.cells:
                if active_owner.get(ref) == tranche_id:
                    del active_owner[ref]
        tranche.state = to_state
    return states, active_owner


def tranche_review_subject(
    tranche_id: str,
    state: TrancheState,
    cells: dict[str, Any],
    results: dict[str, Any],
    deviations: dict[str, Any],
    perf_units: dict[str, Any],
    workloads: dict[str, Any],
    decisions: dict[str, Any],
) -> dict[str, Any] | None:
    if (
        state.checkpoint_event_id is None
        or state.checkpoint_time is None
        or state.repository_commit is None
    ):
        return None

    cell_ids = sorted(state.cells)
    if any(cell_id not in cells or cell_id not in results for cell_id in cell_ids):
        return None

    deviation_ids: set[str] = set()
    perf_ids: set[str] = set()
    decision_ids = set(state.context_decisions)
    for cell_id in cell_ids:
        result = results[cell_id]
        deviation_id = result.get("deviation")
        if deviation_id is not None:
            if not isinstance(deviation_id, str):
                return None
            deviation_ids.add(deviation_id)
        refs = result.get("perf_refs")
        if not isinstance(refs, list) or any(not isinstance(ref, str) for ref in refs):
            return None
        perf_ids.update(refs)
        cell_decisions = cells[cell_id].get("decision_refs")
        if not isinstance(cell_decisions, list) or any(not isinstance(ref, str) for ref in cell_decisions):
            return None
        decision_ids.update(cell_decisions)

    if any(ref not in deviations for ref in deviation_ids):
        return None
    if any(ref not in perf_units for ref in perf_ids):
        return None
    workload_ids = set(state.context_workloads)
    for perf_id in perf_ids:
        workload_id = perf_units[perf_id].get("workload")
        if not isinstance(workload_id, str):
            return None
        workload_ids.add(workload_id)
    if any(ref not in workloads for ref in workload_ids):
        return None
    if any(ref not in decisions for ref in decision_ids):
        return None

    sorted_deviations = sorted(deviation_ids)
    sorted_perf_units = sorted(perf_ids)
    sorted_workloads = sorted(workload_ids)
    sorted_decisions = sorted(decision_ids)
    checkpoint_time = state.checkpoint_time.isoformat().replace("+00:00", "Z")
    return {
        "kind": "tranche-review",
        "version": 1,
        "repository": REPOSITORY,
        "homebrew_commit": HOMEBREW_COMMIT,
        "precedence": PRECEDENCE,
        "tranche": {
            "id": tranche_id,
            "checkpoint_event_id": state.checkpoint_event_id,
            "checkpoint_time": checkpoint_time,
            "repository_commit": state.repository_commit,
            "cells": cell_ids,
            "decision_refs": sorted_decisions,
            "workload_refs": sorted_workloads,
        },
        "scope_cells": [cells[cell_id] for cell_id in cell_ids],
        "results": [results[cell_id] for cell_id in cell_ids],
        "deviations": [deviations[ref] for ref in sorted_deviations],
        "perf_units": [perf_units[ref] for ref in sorted_perf_units],
        "workloads": [workloads[ref] for ref in sorted_workloads],
        "decisions": [decisions[ref] for ref in sorted_decisions],
    }


def subject_digest(
    kind: str,
    subject_id: str,
    scope: dict[str, Any],
    decisions: dict[str, Any],
    deviations: dict[str, Any],
    workloads: dict[str, Any],
    perf_units: dict[str, Any],
    *,
    cells: dict[str, Any] | None = None,
    results: dict[str, Any] | None = None,
    states: dict[str, TrancheState] | None = None,
) -> str | None:
    if kind == "tranche-review":
        state = states.get(subject_id) if states is not None else None
        if state is None or cells is None or results is None:
            return None
        subject = tranche_review_subject(
            subject_id, state, cells, results, deviations, perf_units, workloads, decisions
        )
        return digest(subject) if subject is not None else None
    if kind == "scope" and subject_id == "scope":
        return digest(scope)
    if kind == "product-decision" and subject_id in decisions:
        return digest(decisions[subject_id])
    if kind == "safety-deviation" and subject_id in deviations:
        return deviations[subject_id].get("proof_digest")
    if kind == "performance-floor" and subject_id in perf_units:
        unit = perf_units[subject_id]
        classification = unit.get("classification")
        if not isinstance(classification, dict) or classification.get("grade") not in {"at-floor", "hot"}:
            return None
        return digest(unit.get("floor"))
    if kind == "hot-reclassification" and subject_id in perf_units:
        unit = perf_units[subject_id]
        classification = unit.get("classification")
        if not isinstance(classification, dict) or classification.get("grade") != "hot":
            return None
        workload_id = unit.get("workload")
        workload = workloads.get(workload_id)
        if workload is None:
            return None
        return digest(
            {
                "workload": workload_id,
                "workload_record": workload,
                "samples": unit.get("samples"),
                "input_scaling": unit.get("input_scaling"),
            }
        )
    return None




REVIEW_URL_RE = re.compile(
    r"^/repos/gosuda/Zapbrew/pulls/([1-9][0-9]*)/reviews/([1-9][0-9]*)$"
)


def parse_review_url(url: str) -> tuple[int, int] | None:
    """Return (pull_number, review_id) for a canonical GitHub PR review API URL."""
    parsed = urllib.parse.urlsplit(url)
    if parsed.scheme != "https":
        return None
    if parsed.hostname != "api.github.com":
        return None
    if parsed.port is not None:
        return None
    if "@" in parsed.netloc:
        return None
    if parsed.query or parsed.fragment:
        return None
    match = REVIEW_URL_RE.fullmatch(parsed.path)
    if match is None:
        return None
    return int(match.group(1)), int(match.group(2))


def canonical_tranche_review_line(
    tranche_id: str,
    checkpoint_event_id: str,
    repository_commit: str,
    subject_digest_value: str,
) -> str:
    """Return the exact canonical body line a tranche-review approval must contain."""
    payload = canonical_bytes(
        {
            "checkpoint": checkpoint_event_id,
            "commit": repository_commit,
            "decision": "approve",
            "digest": subject_digest_value,
            "repository": REPOSITORY,
            "tranche": tranche_id,
        }
    ).decode()
    return f"ZAPBREW-TRANCHE-REVIEW v1 {payload}"


def canonical_approval_line(kind: str, subject_id: str, subject_digest_value: str) -> str:
    return (
        f"ZAPBREW-LEDGER-APPROVAL v1 kind={kind} repository={REPOSITORY} "
        f"subject={subject_id} digest={subject_digest_value} homebrew={HOMEBREW_COMMIT} decision=approve"
    )


def default_fetcher(
    url: str, actor: str, token: str
) -> tuple[dict[str, Any], str] | tuple[dict[str, Any], str, dict[str, Any]]:
    headers = {
        "Accept": "application/vnd.github+json",
        "Authorization": f"Bearer {token}",
        "X-GitHub-Api-Version": "2022-11-28",
        "User-Agent": "zapbrew-ledger-verifier-v2",
    }

    def fetch_json(target: str) -> dict[str, Any]:
        request = urllib.request.Request(target, headers=headers)
        with urllib.request.urlopen(request, timeout=20) as response:
            value = json.load(response)
        if not isinstance(value, dict):
            raise ValueError(f"GitHub API response for {target} must be an object")
        return value

    obj = fetch_json(url)
    encoded_actor = urllib.parse.quote(actor, safe="")
    permission_url = f"https://api.github.com/repos/{REPOSITORY}/collaborators/{encoded_actor}/permission"
    permission_obj = fetch_json(permission_url)
    parsed_review = parse_review_url(url)
    if parsed_review is None:
        return obj, permission_obj.get("permission", "none")
    pull_number, _ = parsed_review
    pr_obj = fetch_json(f"https://api.github.com/repos/{REPOSITORY}/pulls/{pull_number}")
    return obj, permission_obj.get("permission", "none"), pr_obj


def _fetch_parts(
    fetched: tuple[Any, ...],
) -> tuple[dict[str, Any], str, dict[str, Any] | None]:
    if not isinstance(fetched, tuple) or len(fetched) not in {2, 3}:
        raise ValueError("fetcher must return (object, permission) or (object, permission, pull_request)")
    obj, permission = fetched[:2]
    pr_obj = fetched[2] if len(fetched) == 3 else None
    if not isinstance(obj, dict) or not isinstance(permission, str):
        raise ValueError("fetcher returned invalid object or permission")
    if pr_obj is not None and not isinstance(pr_obj, dict):
        raise ValueError("fetcher returned invalid pull request object")
    return obj, permission, pr_obj


def validate_approvals(
    events: list[Any],
    scope: dict[str, Any],
    decisions: dict[str, Any],
    deviations: dict[str, Any],
    workloads: dict[str, Any],
    perf_units: dict[str, Any],
    offline: bool,
    report: Report,
    fetcher: Callable[[str, str, str], tuple[Any, ...]] | None = None,
    *,
    cells: dict[str, Any] | None = None,
    results: dict[str, Any] | None = None,
    states: dict[str, TrancheState] | None = None,
) -> tuple[set[tuple[str, str]], set[str], dict[str, dict[str, Any]], set[str]]:
    """Authenticate current approvals and return their structural/authentication indexes."""
    event_ids: set[str] = set()
    review_events: dict[str, dict[str, Any]] = {}
    current_events: list[dict[str, Any]] = []
    stale_keys: set[tuple[str, str]] = set()

    for index, event in enumerate(events):
        label = f"approvals.jsonl event {index + 1}"
        if not isinstance(event, dict) or set(event) != APPROVAL_FIELDS:
            report.integrity.append(f"{label} has non-canonical fields")
            continue
        event_id = event.get("event_id")
        if not isinstance(event_id, str) or not event_id or event_id in event_ids:
            report.integrity.append(f"{label} has missing or duplicate event_id")
            continue
        event_ids.add(event_id)
        kind = event.get("kind")
        subject_id = event.get("subject_id")
        if kind not in APPROVAL_KINDS:
            report.integrity.append(f"{label} has invalid kind")
            continue
        if not isinstance(subject_id, str) or not subject_id:
            report.integrity.append(f"{label}.subject_id must be non-empty")
            continue
        if kind == "tranche-review":
            review_events[event_id] = event
        if event.get("repository") != REPOSITORY:
            report.integrity.append(f"{label} repository mismatch")
            continue
        if event.get("homebrew_commit") != HOMEBREW_COMMIT:
            report.integrity.append(f"{label} Homebrew commit mismatch")
            continue
        actor = event.get("actor")
        if not isinstance(actor, str) or not actor:
            report.integrity.append(f"{label}.actor must be non-empty")
            continue
        url = event.get("object_url")
        if not isinstance(url, str):
            report.integrity.append(f"{label}.object_url must be a repository GitHub API URL")
            continue
        if kind == "tranche-review":
            if parse_review_url(url) is None:
                report.integrity.append(f"{label}.object_url must be a canonical PR review API URL")
                continue
        elif not url.startswith(f"https://api.github.com/repos/{REPOSITORY}/"):
            report.integrity.append(f"{label}.object_url must be a repository GitHub API URL")
            continue
        if not valid_sha256(event.get("object_body_digest")):
            report.integrity.append(f"{label}.object_body_digest must be sha256")
            continue
        if parse_time(event.get("event_time"), f"{label}.event_time", report) is None:
            continue
        expected_subject_digest = subject_digest(
            kind, subject_id, scope, decisions, deviations, workloads, perf_units,
            cells=cells, results=results, states=states,
        )
        if expected_subject_digest is None:
            report.integrity.append(f"{label} references unknown or mismatched subject")
            continue
        if event.get("subject_digest") != expected_subject_digest:
            stale_keys.add((kind, subject_id))
            continue
        current_events.append(event)

    if offline:
        for event in current_events:
            report.incomplete.append(f"approval {event['kind']}:{event['subject_id']} requires online authority proof")
        for key in stale_keys:
            report.incomplete.append(f"approval {key[0]}:{key[1]} subject digest is stale")
        return set(), event_ids, review_events, set()

    token = os.environ.get("GITHUB_TOKEN", "")
    if not token and fetcher is None:
        for event in current_events:
            report.incomplete.append(f"approval {event['kind']}:{event['subject_id']} cannot authenticate without GITHUB_TOKEN")
        for key in stale_keys:
            report.incomplete.append(f"approval {key[0]}:{key[1]} cannot authenticate without GITHUB_TOKEN")
        return set(), event_ids, review_events, set()

    approved: set[tuple[str, str]] = set()
    authenticated_review_ids: set[str] = set()
    pending: dict[tuple[str, str], list[str]] = {}
    fetch = fetcher or default_fetcher
    caught = (OSError, TimeoutError, urllib.error.URLError, urllib.error.HTTPError, ValueError, json.JSONDecodeError)

    for event in current_events:
        key = (event["kind"], event["subject_id"])
        if key in approved:
            continue
        label = f"approval {key[0]}:{key[1]}"
        try:
            obj, permission, pr_obj = _fetch_parts(fetch(event["object_url"], event["actor"], token))
        except caught as exc:
            pending.setdefault(key, []).append(f"online authority proof unavailable: {exc}")
            continue
        user = obj.get("user")
        object_actor = user.get("login") if isinstance(user, dict) else None
        body = obj.get("body")
        created_raw = obj.get("submitted_at") if key[0] == "tranche-review" else obj.get("created_at", obj.get("submitted_at"))
        if object_actor != event["actor"]:
            pending.setdefault(key, []).append("object actor mismatch")
            continue
        if not isinstance(body, str):
            pending.setdefault(key, []).append("object body missing")
            continue
        if text_digest(body) != event["object_body_digest"]:
            pending.setdefault(key, []).append("object body digest is stale")
            continue
        if key[0] == "tranche-review":
            state = states.get(key[1]) if states is not None else None
            parsed_review = parse_review_url(event["object_url"])
            if state is None or parsed_review is None:
                pending.setdefault(key, []).append("review checkpoint is unavailable")
                continue
            pull_number, review_id = parsed_review
            expected_line = canonical_tranche_review_line(
                key[1], state.checkpoint_event_id or "", state.repository_commit or "", event["subject_digest"]
            )
            if body != expected_line:
                pending.setdefault(key, []).append("review body is not the exact canonical tranche-review line")
                continue
            pull_url = f"https://api.github.com/repos/{REPOSITORY}/pulls/{pull_number}"
            if obj.get("id") != review_id or obj.get("pull_request_url") != pull_url:
                pending.setdefault(key, []).append("review URL/object identity mismatch")
                continue
            if pr_obj is None or pr_obj.get("number") != pull_number:
                pending.setdefault(key, []).append("pull request identity unavailable or mismatched")
                continue
            pr_base = pr_obj.get("base")
            pr_head = pr_obj.get("head")
            if not isinstance(pr_base, dict) or pr_base.get("ref") != "main":
                pending.setdefault(key, []).append("pull request base is not main")
                continue
            if not isinstance(pr_head, dict) or pr_head.get("sha") != state.repository_commit:
                pending.setdefault(key, []).append("pull request head does not match checkpoint")
                continue
            if obj.get("state") != "APPROVED":
                pending.setdefault(key, []).append("review state is not APPROVED")
                continue
            pr_user = pr_obj.get("user")
            if not isinstance(pr_user, dict) or not isinstance(pr_user.get("login"), str):
                pending.setdefault(key, []).append("pull request author missing")
                continue
            if pr_user["login"].casefold() == event["actor"].casefold():
                pending.setdefault(key, []).append("reviewer is the pull request author")
                continue
            if obj.get("commit_id") != state.repository_commit:
                pending.setdefault(key, []).append("reviewed commit does not match checkpoint")
                continue
            submitted = parse_time_soft(created_raw)
            event_time = parse_time_soft(event.get("event_time"))
            if submitted is None:
                pending.setdefault(key, []).append("review submitted_at is not a valid RFC3339 timestamp")
                continue
            if state.checkpoint_time is None or submitted < state.checkpoint_time:
                report.integrity.append(f"{label} review was submitted before its checkpoint")
                continue
            if event_time is None or submitted > event_time or state.approved_at is None or submitted > state.approved_at:
                report.integrity.append(f"{label} review was submitted after its approval transition")
                continue
            if permission not in {"maintain", "admin"}:
                pending.setdefault(key, []).append("actor lacks current maintain/admin permission")
                continue
            authenticated_review_ids.add(event["event_id"])
        else:
            expected_line = canonical_approval_line(event["kind"], event["subject_id"], event["subject_digest"])
            if expected_line not in body.splitlines():
                pending.setdefault(key, []).append("object body lacks the exact canonical approval line")
                continue
            object_created = parse_time_soft(created_raw)
            event_time = parse_time(event.get("event_time"), f"{label} event_time", report)
            if object_created is None:
                pending.setdefault(key, []).append("object created_at is not a valid RFC3339 timestamp")
                continue
            if event_time is not None and object_created > event_time:
                report.integrity.append(f"{label} object was created after the ledger event")
                continue
            if permission not in {"maintain", "admin"}:
                pending.setdefault(key, []).append("actor lacks current maintain/admin permission")
                continue
        approved.add(key)

    for key in stale_keys:
        if key not in approved:
            pending.setdefault(key, []).append("subject digest is stale")
    for key, reasons in pending.items():
        if key not in approved:
            report.incomplete.append(f"approval {key[0]}:{key[1]} {reasons[-1]}")
    return approved, event_ids, review_events, authenticated_review_ids


def bind_tranche_reviews(
    states: dict[str, TrancheState],
    all_event_ids: set[str],
    review_events: dict[str, dict[str, Any]],
    authenticated_review_ids: set[str],
    report: Report,
) -> None:
    for tranche_id, state in states.items():
        state.reviewed = False
        if state.review_ref is None:
            continue
        if state.review_ref not in all_event_ids:
            report.integrity.append(f"tranche {tranche_id} review_ref names no approval event")
            continue
        event = review_events.get(state.review_ref)
        if event is None:
            report.integrity.append(f"tranche {tranche_id} review_ref names a non-tranche-review approval")
            continue
        if event.get("subject_id") != tranche_id:
            report.integrity.append(f"tranche {tranche_id} review_ref names another tranche's review")
            continue
        event_time = parse_time_soft(event.get("event_time"))
        if state.approved_at is None or event_time is None or event_time > state.approved_at:
            report.integrity.append(f"tranche {tranche_id} review_ref names a late approval event")
            continue
        if event.get("event_id") in authenticated_review_ids:
            state.reviewed = True


def validate_landed_checkpoints(
    ledger_root: Path,
    states: dict[str, TrancheState],
    review_events: dict[str, dict[str, Any]],
    report: Report,
    fetcher: Callable[[str, str, str], tuple[Any, ...]] | None,
) -> None:
    token = os.environ.get("GITHUB_TOKEN", "")
    if not token and fetcher is None:
        for tranche_id, state in states.items():
            if state.landed and state.reviewed:
                report.incomplete.append(f"tranche {tranche_id} landed checkpoint requires GITHUB_TOKEN")
        return

    def fetch_object(url: str, label: str) -> dict[str, Any] | None:
        if fetcher is not None:
            try:
                fetched = fetcher(url, "", token)
            except Exception as exc:
                report.incomplete.append(f"{label} is unreachable: {exc}")
                return None
            try:
                obj, _, _ = _fetch_parts(fetched)
            except ValueError as exc:
                report.integrity.append(f"{label} response is malformed: {exc}")
                return None
            return obj

        request = urllib.request.Request(
            url,
            headers={"Authorization": f"Bearer {token}", "Accept": "application/vnd.github+json"},
        )
        try:
            with urllib.request.urlopen(request, timeout=20) as response:
                body = response.read()
        except (OSError, TimeoutError, urllib.error.URLError, urllib.error.HTTPError) as exc:
            report.incomplete.append(f"{label} is unreachable: {exc}")
            return None
        try:
            obj = json.loads(body)
        except (UnicodeDecodeError, json.JSONDecodeError) as exc:
            report.integrity.append(f"{label} response is malformed: {exc}")
            return None
        if not isinstance(obj, dict):
            report.integrity.append(f"{label} response must be an object")
            return None
        return obj

    def committed_file(repository_commit: str, name: str, label: str) -> Any | None:
        encoded_commit = urllib.parse.quote(repository_commit, safe="")
        url = f"https://api.github.com/repos/{REPOSITORY}/contents/.outline/ledger/{name}?ref={encoded_commit}"
        obj = fetch_object(url, label)
        if obj is None:
            return None
        if obj.get("encoding") != "base64" or not isinstance(obj.get("content"), str):
            report.integrity.append(f"{label} must contain base64 content")
            return None
        try:
            encoded = obj["content"].encode("ascii")
            text = base64.b64decode(b"".join(encoded.split()), validate=True).decode("utf-8")
            if name == "tranches.jsonl":
                return [json.loads(line) for line in text.splitlines() if line.strip()]
            return json.loads(text)
        except (ValueError, UnicodeDecodeError, json.JSONDecodeError) as exc:
            report.integrity.append(f"{label} content is malformed: {exc}")
            return None

    for tranche_id, state in states.items():
        if not state.landed or not state.reviewed:
            continue
        repository_commit = state.repository_commit
        checkpoint_event_id = state.checkpoint_event_id
        review_event = review_events.get(state.review_ref or "")
        if repository_commit is None or checkpoint_event_id is None or review_event is None:
            report.integrity.append(f"tranche {tranche_id} landed review binding is incomplete")
            continue

        repository = fetch_object(f"https://api.github.com/repos/{REPOSITORY}", f"tranche {tranche_id} repository")
        if repository is None:
            continue
        default_branch = repository.get("default_branch")
        if default_branch != "main":
            report.integrity.append(f"tranche {tranche_id} repository default_branch is not main")
            continue
        compare_url = (
            f"https://api.github.com/repos/{REPOSITORY}/compare/{repository_commit}...main"
        )
        comparison = fetch_object(compare_url, f"tranche {tranche_id} checkpoint comparison")
        if comparison is None:
            continue
        status = comparison.get("status")
        if status is None:
            report.incomplete.append(f"tranche {tranche_id} checkpoint comparison has no reachable status")
            continue
        if status not in {"ahead", "identical"}:
            report.integrity.append(
                f"tranche {tranche_id} checkpoint {repository_commit} is not on the default branch ({status!r})"
            )
            continue

        committed_scope = committed_file(repository_commit, "scope.json", f"tranche {tranche_id} committed scope.json")
        committed_results = committed_file(
            repository_commit, "results.json", f"tranche {tranche_id} committed results.json"
        )
        committed_events = committed_file(
            repository_commit, "tranches.jsonl", f"tranche {tranche_id} committed tranches.jsonl"
        )
        if committed_scope is None or committed_results is None or committed_events is None:
            continue

        committed_report = Report()
        committed_cells, committed_decisions, _ = validate_scope(committed_scope, None, committed_report)
        (
            committed_cell_results,
            committed_deviations,
            committed_workloads,
            committed_perf_units,
            _,
            _,
        ) = validate_results(committed_results, committed_cells, committed_report)
        committed_states, _ = replay_tranches(
            committed_events, committed_cells, committed_decisions, committed_workloads, committed_report
        )
        if committed_report.integrity:
            report.integrity.append(
                f"tranche {tranche_id} committed ledger closure is invalid: {'; '.join(committed_report.integrity)}"
            )
            continue
        if not any(
            isinstance(event, dict) and event.get("event_id") == checkpoint_event_id for event in committed_events
        ):
            report.integrity.append(f"tranche {tranche_id} committed tranches log lacks checkpoint {checkpoint_event_id}")
            continue
        committed_state = committed_states.get(tranche_id)
        if (
            committed_state is None
            or committed_state.checkpoint_event_id != checkpoint_event_id
            or committed_state.repository_commit != repository_commit
        ):
            report.integrity.append(f"tranche {tranche_id} committed checkpoint state does not match its review")
            continue
        subject = tranche_review_subject(
            tranche_id,
            committed_state,
            committed_cells,
            committed_cell_results,
            committed_deviations,
            committed_perf_units,
            committed_workloads,
            committed_decisions,
        )
        if subject is None or digest(subject) != review_event.get("subject_digest"):
            report.integrity.append(f"tranche {tranche_id} committed closure does not match its reviewed subject")


def validate_completion(
    scope: dict[str, Any],
    cells: dict[str, Any],
    decisions: dict[str, Any],
    results: dict[str, Any],
    deviations: dict[str, Any],
    states: dict[str, TrancheState],
    active_owner: dict[str, str],
    approvals: set[tuple[str, str]],
    workloads: dict[str, Any],
    perf_units: dict[str, Any],
    hot_markers: dict[str, dict[str, Any]],
    report: Report,
) -> None:
    if ("scope", "scope") not in approvals:
        report.incomplete.append("missing current repository-maintainer scope approval")

    for decision_id, decision in decisions.items():
        if decision.get("status") == "resolved" and ("product-decision", decision_id) not in approvals:
            report.incomplete.append(f"resolved decision {decision_id} lacks current approval")

    for cell_id, cell in cells.items():
        result = results.get(cell_id)
        if result is None:
            continue
        finding = result.get("finding")
        if finding in {"missing", "defect"}:
            report.incomplete.append(f"cell {cell_id} finding={finding}")
            continue
        achieved = result.get("achieved_evidence")
        achieved_kinds = {
            item.get("kind") for item in achieved if isinstance(item, dict)
        } if isinstance(achieved, list) else set()
        required = set(cell.get("expected_evidence", []))
        missing_evidence = sorted(required - achieved_kinds)
        if missing_evidence:
            report.incomplete.append(f"cell {cell_id} missing evidence {missing_evidence}")

        effect = cell.get("effect")
        failure = result.get("failure_evidence")
        failure_ok = True
        if effect in MUTATION_EFFECTS:
            failure_ok = isinstance(failure, dict) and failure.get("status") == "proved"
            if not failure_ok:
                report.incomplete.append(f"mutation cell {cell_id} lacks resolved failure evidence")
        if isinstance(failure, dict) and failure.get("status") in {"defect", "unresolved"}:
            report.incomplete.append(
                f"cell {cell_id} failure evidence is {failure.get('status')}"
            )

        native_hosts_ok = True
        if cell.get("platform") == "any" and effect in MUTATION_EFFECTS:
            native_hosts = {
                item.get("platform")
                for item in achieved
                if isinstance(item, dict) and item.get("kind") in NATIVE_PROOF_KINDS
            } if isinstance(achieved, list) else set()
            missing_hosts = sorted({"linux", "macos"} - native_hosts)
            if missing_hosts:
                native_hosts_ok = False
                report.incomplete.append(
                    f"platform-any mutation cell {cell_id} lacks native proof on {missing_hosts}"
                )

        expected = cell.get("expected_disposition")
        disposition_ok = False
        if expected == "must-match":
            disposition_ok = not missing_evidence and failure_ok and native_hosts_ok
        elif expected == "must-be-outside-architecture":
            disposition_ok = not missing_evidence and failure_ok and native_hosts_ok
        elif expected == "safety-deviation-candidate":
            deviation_id = result.get("deviation")
            disposition_ok = (
                not missing_evidence
                and failure_ok
                and native_hosts_ok
                and deviation_id in deviations
                and ("safety-deviation", deviation_id) in approvals
            )
            if not disposition_ok:
                report.incomplete.append(f"cell {cell_id} lacks an approved safety deviation")
        elif expected == "product-decision-pending":
            report.incomplete.append(f"cell {cell_id} remains product-decision-pending")

        owner = active_owner.get(cell_id)
        landed = owner is not None and states[owner].state == "landed" and states[owner].reviewed
        if not landed:
            report.incomplete.append(f"cell {cell_id} is not in a reviewed landed tranche")
        if not disposition_ok:
            report.incomplete.append(f"cell {cell_id} has no completing disposition")

    for tranche_id, state in states.items():
        if state.state not in {"landed", "reverted"} and state.cells:
            report.incomplete.append(f"tranche {tranche_id} is {state.state}, not landed")

    for unit_id, marker in hot_markers.items():
        reclass_key = ("hot-reclassification", unit_id)
        if reclass_key in approvals:
            report.notes.append(
                f"hot performance unit {unit_id} is reclassified cold by authenticated approval"
            )
            continue
        floor_key = ("performance-floor", unit_id)
        if floor_key not in approvals:
            report.incomplete.append(
                f"hot performance unit {unit_id} lacks authenticated performance-floor approval"
            )
        if marker["multiple"] <= 2:
            continue
        report.incomplete.append(f"hot performance unit {unit_id} is {marker['multiple']:.6g}x floor (>2x)")
        if marker["action"] == "revert" and marker["pre_median"] is not None and marker["post_median"] is not None:
            win = marker["pre_median"] / marker["post_median"]
            if win < 1.05:
                report.incomplete.append(
                    f"hot above-floor unit {unit_id} remains incomplete after no-win revert ({win:.6g}x)"
                )


def verify(
    ledger_root: Path,
    offline: bool,
    fetcher: Callable[[str, str, str], tuple[Any, ...]] | None = None,
) -> Report:
    report = Report()
    scope = load_json(ledger_root / "scope.json", "scope.json", report)
    result_data = load_json(ledger_root / "results.json", "results.json", report)
    approval_events = load_jsonl(ledger_root / "approvals.jsonl", "approvals.jsonl", report)
    tranche_events = load_jsonl(ledger_root / "tranches.jsonl", "tranches.jsonl", report)
    if report.integrity:
        return report

    cells, decisions, _ = validate_scope(scope, ledger_root, report)
    results, deviations, workloads, perf_units, _, hot_markers = validate_results(result_data, cells, report)
    states, active_owner = replay_tranches(tranche_events, cells, decisions, workloads, report)
    approvals, approval_event_ids, review_events, authenticated_review_ids = validate_approvals(
        approval_events,
        scope,
        decisions,
        deviations,
        workloads,
        perf_units,
        offline,
        report,
        fetcher,
        cells=cells,
        results=results,
        states=states,
    )
    tranche_event_ids = {
        event.get("event_id")
        for event in tranche_events
        if isinstance(event, dict) and isinstance(event.get("event_id"), str)
    }
    duplicate_event_ids = sorted(approval_event_ids & tranche_event_ids)
    if duplicate_event_ids:
        report.integrity.append(
            f"event IDs must be globally unique across approval and tranche logs: {duplicate_event_ids}"
        )
    bind_tranche_reviews(states, approval_event_ids, review_events, authenticated_review_ids, report)
    if report.integrity:
        return report
    validate_landed_checkpoints(ledger_root, states, review_events, report, fetcher)
    if report.integrity:
        return report
    validate_completion(
        scope,
        cells,
        decisions,
        results,
        deviations,
        states,
        active_owner,
        approvals,
        workloads,
        perf_units,
        hot_markers,
        report,
    )
    report.notes.append(
        f"scope files={len(scope.get('files', []))} cells={len(cells)} results={len(results)}"
    )
    return report


def write_json(path: Path, value: Any) -> None:
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n")


def make_temp_ledger(source: Path, temp_parent: Path) -> Path:
    repo = temp_parent / "repo"
    ledger = repo / ".outline" / "ledger"
    ledger.parent.mkdir(parents=True)
    shutil.copytree(source, ledger)
    scope = json.loads((ledger / "scope.json").read_text())
    for row in scope["files"]:
        target = repo / row["path"]
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text("// self-test fixture\n")
    results = json.loads((ledger / "results.json").read_text())
    for row in results["results"]:
        row.update(
            achieved_evidence=[],
            failure_evidence=None,
            perf_refs=[],
            deviation=None,
            finding="none",
        )
    results["deviations"] = []
    results["workloads"] = []
    results["perf_units"] = []
    write_json(ledger / "results.json", results)
    (ledger / "approvals.jsonl").write_text("")
    (ledger / "tranches.jsonl").write_text("")
    return ledger


def run_self_tests(source: Path) -> tuple[bool, list[str]]:
    cases: list[tuple[str, Callable[[Path], tuple[bool, str]]]] = []

    def case(name: str) -> Callable[[Callable[[Path], tuple[bool, str]]], Callable[[Path], tuple[bool, str]]]:
        def register(fn: Callable[[Path], tuple[bool, str]]) -> Callable[[Path], tuple[bool, str]]:
            cases.append((name, fn))
            return fn
        return register

    @case("missing file row -> exit 2")
    def missing_file(ledger: Path) -> tuple[bool, str]:
        scope = json.loads((ledger / "scope.json").read_text())
        scope["files"].pop()
        write_json(ledger / "scope.json", scope)
        result = verify(ledger, offline=True)
        return result.code == 2 and any("file universe mismatch" in x for x in result.integrity), result.render()

    @case("placeholder Homebrew anchor -> exit 2")
    def placeholder_homebrew_anchor(ledger: Path) -> tuple[bool, str]:
        scope = json.loads((ledger / "scope.json").read_text())
        scope["cells"][0]["source_pair"]["homebrew"] = "pending/unresolved"
        write_json(ledger / "scope.json", scope)
        result = verify(ledger, offline=True)
        return (
            result.code == 2
            and any("unresolved Homebrew source anchor" in item for item in result.integrity)
        ), result.render()

    @case("duplicate result -> exit 2")
    def duplicate_result(ledger: Path) -> tuple[bool, str]:
        data = json.loads((ledger / "results.json").read_text())
        data["results"].append(copy.deepcopy(data["results"][0]))
        write_json(ledger / "results.json", data)
        result = verify(ledger, offline=True)
        return result.code == 2 and any("results.results has duplicate cell" in x for x in result.integrity), result.render()

    @case("matched cell and result shrinkage -> exit 2")
    def matched_cell_result_shrinkage(ledger: Path) -> tuple[bool, str]:
        scope = json.loads((ledger / "scope.json").read_text())
        data = json.loads((ledger / "results.json").read_text())
        removed = scope["cells"].pop()["cell_id"]
        data["results"] = [row for row in data["results"] if row["cell"] != removed]
        write_json(ledger / "scope.json", scope)
        write_json(ledger / "results.json", data)
        result = verify(ledger, offline=True)
        marker = f"scope must contain exactly {CANONICAL_CELL_COUNT} canonical cells"
        return result.code == 2 and any(marker in item for item in result.integrity), result.render()

    @case("missing macOS mutation tier -> exit 2")
    def missing_macos_tier(ledger: Path) -> tuple[bool, str]:
        scope = json.loads((ledger / "scope.json").read_text())
        target = next(
            cell for cell in scope["cells"]
            if cell["platform"] == "macos"
            and cell["effect"] == "filesystem-mutation"
            and "cross-compile" in cell["expected_evidence"]
        )
        target["expected_evidence"].remove("cross-compile")
        write_json(ledger / "scope.json", scope)
        result = verify(ledger, offline=True)
        return result.code == 2 and any("macOS filesystem mutation" in x for x in result.integrity), result.render()

    @case("mutation complete without failure -> exit 1")
    def mutation_without_failure(ledger: Path) -> tuple[bool, str]:
        scope = json.loads((ledger / "scope.json").read_text())
        data = json.loads((ledger / "results.json").read_text())
        cell = next(cell for cell in scope["cells"] if cell["effect"] in MUTATION_EFFECTS)
        result_row = next(row for row in data["results"] if row["cell"] == cell["cell_id"])
        result_row["achieved_evidence"] = [
            {
                "id": f"self-{index}",
                "kind": kind,
                "platform": cell["platform"],
                "anchor": "self-test",
                "digest": "a" * 64,
            }
            for index, kind in enumerate(cell["expected_evidence"])
        ]
        write_json(ledger / "results.json", data)
        verification = verify(ledger, offline=True)
        marker = f"mutation cell {cell['cell_id']} lacks resolved failure evidence"
        return verification.code == 1 and marker in verification.incomplete, verification.render()

    @case("building to landed jump -> exit 2")
    def illegal_edge(ledger: Path) -> tuple[bool, str]:
        scope = json.loads((ledger / "scope.json").read_text())
        cell = scope["cells"][0]["cell_id"]
        events = [
            {"event_id":"t1","tranche_id":"tranche-1","from":"proposed","to":"scoped","event_time":"2026-01-01T00:00:00Z","cells":[cell]},
            {"event_id":"t2","tranche_id":"tranche-1","from":"scoped","to":"building","event_time":"2026-01-01T00:01:00Z"},
            {"event_id":"t3","tranche_id":"tranche-1","from":"building","to":"landed","event_time":"2026-01-01T00:02:00Z"},
        ]
        (ledger / "tranches.jsonl").write_text("".join(json.dumps(row)+"\n" for row in events))
        result = verify(ledger, offline=True)
        return result.code == 2 and any("illegal tranche edge" in x for x in result.integrity), result.render()

    @case("early or malformed review ref cannot confer review -> exit 2")
    def invalid_review_ref(ledger: Path) -> tuple[bool, str]:
        scope = json.loads((ledger / "scope.json").read_text())
        cell = scope["cells"][0]["cell_id"]
        variants = [
            ("early", 0, "review-1"),
            ("numeric", 4, 1),
            ("boolean", 4, True),
        ]
        failures: list[str] = []
        for name, review_index, review_ref in variants:
            events: list[dict[str, object]] = [
                {"event_id":"t1","tranche_id":"tranche-1","from":"proposed","to":"scoped","event_time":"2026-01-01T00:00:00Z","cells":[cell]},
                {"event_id":"t2","tranche_id":"tranche-1","from":"scoped","to":"building","event_time":"2026-01-01T00:01:00Z"},
                {"event_id":"t3","tranche_id":"tranche-1","from":"building","to":"evidence-pending","event_time":"2026-01-01T00:02:00Z"},
                {"event_id":"t4","tranche_id":"tranche-1","from":"evidence-pending","to":"review-pending","event_time":"2026-01-01T00:03:00Z","repository_commit":"a" * 40},
                {"event_id":"t5","tranche_id":"tranche-1","from":"review-pending","to":"approved","event_time":"2026-01-01T00:04:00Z","review_ref":"review-1"},
                {"event_id":"t6","tranche_id":"tranche-1","from":"approved","to":"landed","event_time":"2026-01-01T00:05:00Z"},
            ]
            events[review_index]["review_ref"] = review_ref
            (ledger / "tranches.jsonl").write_text("".join(json.dumps(row) + "\n" for row in events))
            result = verify(ledger, offline=True)
            if result.code != 2 or not any("review_ref" in item for item in result.integrity):
                failures.append(f"{name}: {result.render()}")
        return not failures, "\n".join(failures)

    def proposed_scoped_event(ledger: Path, **optional: Any) -> dict[str, Any]:
        scope = json.loads((ledger / "scope.json").read_text())
        return {
            "event_id": "t1",
            "tranche_id": "tranche-1",
            "from": "proposed",
            "to": "scoped",
            "event_time": "2026-01-01T00:00:00Z",
            "cells": [scope["cells"][0]["cell_id"]],
            **optional,
        }

    def verify_tranche_event(ledger: Path, event: dict[str, Any]) -> Report:
        (ledger / "tranches.jsonl").write_text(json.dumps(event) + "\n")
        return verify(ledger, offline=True)

    @case("unknown workload ref -> exit 2")
    def unknown_workload_ref(ledger: Path) -> tuple[bool, str]:
        result = verify_tranche_event(
            ledger, proposed_scoped_event(ledger, workload_refs=["unknown-workload"])
        )
        return (
            result.code == 2
            and any("workload_refs references unknown IDs" in item for item in result.integrity)
        ), result.render()

    @case("duplicate decision refs -> exit 2")
    def duplicate_decision_refs(ledger: Path) -> tuple[bool, str]:
        result = verify_tranche_event(
            ledger, proposed_scoped_event(ledger, decision_refs=["D2-linux-cask-subset"] * 2)
        )
        return (
            result.code == 2
            and any("decision_refs must contain unique references" in item for item in result.integrity)
        ), result.render()

    @case("non-string decision ref -> exit 2")
    def non_string_decision_ref(ledger: Path) -> tuple[bool, str]:
        result = verify_tranche_event(
            ledger, proposed_scoped_event(ledger, decision_refs=["D2-linux-cask-subset", 2])
        )
        return (
            result.code == 2
            and any("decision_refs must be a non-empty array" in item for item in result.integrity)
        ), result.render()

    @case("malformed repository commit -> exit 2")
    def malformed_repository_commit(ledger: Path) -> tuple[bool, str]:
        result = verify_tranche_event(
            ledger, proposed_scoped_event(ledger, repository_commit="A" * 40)
        )
        return (
            result.code == 2
            and any("repository_commit must be one lowercase" in item for item in result.integrity)
        ), result.render()

    @case("duplicate scoped cells -> exit 2")
    def duplicate_scoped_cells(ledger: Path) -> tuple[bool, str]:
        event = proposed_scoped_event(ledger)
        event["cells"] *= 2
        result = verify_tranche_event(ledger, event)
        return (
            result.code == 2
            and any("cells must contain unique references" in item for item in result.integrity)
        ), result.render()

    @case("empty scoped cells -> exit 2")
    def empty_scoped_cells(ledger: Path) -> tuple[bool, str]:
        result = verify_tranche_event(ledger, proposed_scoped_event(ledger, cells=[]))
        return (
            result.code == 2
            and any("cells must be a non-empty array" in item for item in result.integrity)
        ), result.render()

    @case("unpinned safety-deviation anchor -> exit 2")
    def unpinned_safety_deviation_anchor(ledger: Path) -> tuple[bool, str]:
        scope = json.loads((ledger / "scope.json").read_text())
        cell = next(
            cell
            for cell in scope["cells"]
            if cell["expected_disposition"] == "safety-deviation-candidate"
        )
        cell["source_pair"]["homebrew"] = (
            "https://github.com/Homebrew/brew/blob/main/Library/Homebrew/example.rb"
        )
        write_json(ledger / "scope.json", scope)
        result = verify(ledger, offline=True)
        return (
            result.code == 2
            and any("safety-deviation-candidate anchor is not pinned" in item for item in result.integrity)
        ), result.render()

    @case("unrelated/malformed approval body -> exit 1")
    def malformed_approval(ledger: Path) -> tuple[bool, str]:
        scope = json.loads((ledger / "scope.json").read_text())
        body = "This comment approves something unrelated."
        event = {
            "event_id":"approval-1",
            "kind":"scope",
            "subject_id":"scope",
            "subject_digest":digest(scope),
            "homebrew_commit":HOMEBREW_COMMIT,
            "repository":REPOSITORY,
            "actor":"maintainer",
            "object_url":f"https://api.github.com/repos/{REPOSITORY}/issues/comments/1",
            "object_body_digest":text_digest(body),
            "event_time":"2026-01-01T00:01:00Z",
        }
        (ledger / "approvals.jsonl").write_text(json.dumps(event)+"\n")
        def fetcher(url: str, actor: str, token: str) -> tuple[dict[str, Any], str]:
            return {"user":{"login":"maintainer"},"body":body,"created_at":"2026-01-01T00:00:00Z"}, "admin"
        result = verify(ledger, offline=False, fetcher=fetcher)
        return result.code == 1 and any("canonical approval line" in x for x in result.incomplete), result.render()

    @case("stale historical approval no longer exit 2 -> exit 1")
    def stale_historical(ledger: Path) -> tuple[bool, str]:
        scope = json.loads((ledger / "scope.json").read_text())
        scope["cells"][0]["behavior"] += " "
        stale_scope = copy.deepcopy(scope)
        stale_body = canonical_approval_line("scope", "scope", digest(stale_scope))
        event = {
            "event_id":"approval-1",
            "kind":"scope",
            "subject_id":"scope",
            "subject_digest":digest(stale_scope),
            "homebrew_commit":HOMEBREW_COMMIT,
            "repository":REPOSITORY,
            "actor":"maintainer",
            "object_url":f"https://api.github.com/repos/{REPOSITORY}/issues/comments/1",
            "object_body_digest":text_digest(stale_body),
            "event_time":"2026-01-01T00:00:00Z",
        }
        (ledger / "approvals.jsonl").write_text(json.dumps(event)+"\n")
        def fetcher(url: str, actor: str, token: str) -> tuple[dict[str, Any], str]:
            return {"user":{"login":"maintainer"},"body":stale_body,"created_at":"2026-01-01T00:00:00Z"}, "admin"
        result = verify(ledger, offline=False, fetcher=fetcher)
        return result.code == 1 and any("subject digest is stale" in x for x in result.incomplete), result.render()

    @case("stale old plus current new reapproval -> exit 1")
    def stale_then_fresh(ledger: Path) -> tuple[bool, str]:
        scope = json.loads((ledger / "scope.json").read_text())
        stale_scope = copy.deepcopy(scope)
        stale_scope["cells"][0]["behavior"] += " "
        stale_body = canonical_approval_line("scope", "scope", digest(stale_scope))
        stale_event = {
            "event_id":"approval-1",
            "kind":"scope",
            "subject_id":"scope",
            "subject_digest":digest(stale_scope),
            "homebrew_commit":HOMEBREW_COMMIT,
            "repository":REPOSITORY,
            "actor":"maintainer",
            "object_url":f"https://api.github.com/repos/{REPOSITORY}/issues/comments/1",
            "object_body_digest":text_digest(stale_body),
            "event_time":"2026-01-01T00:00:00Z",
        }
        fresh_body = canonical_approval_line("scope", "scope", digest(scope))
        fresh_event = {
            "event_id":"approval-2",
            "kind":"scope",
            "subject_id":"scope",
            "subject_digest":digest(scope),
            "homebrew_commit":HOMEBREW_COMMIT,
            "repository":REPOSITORY,
            "actor":"maintainer",
            "object_url":f"https://api.github.com/repos/{REPOSITORY}/issues/comments/2",
            "object_body_digest":text_digest(fresh_body),
            "event_time":"2026-01-02T00:00:00Z",
        }
        (ledger / "approvals.jsonl").write_text(
            json.dumps(stale_event)+"\n"+json.dumps(fresh_event)+"\n"
        )
        def fetcher(url: str, actor: str, token: str) -> tuple[dict[str, Any], str]:
            body = stale_body if "comments/1" in url else fresh_body
            created = "2026-01-01T00:00:00Z" if "comments/1" in url else "2026-01-02T00:00:00Z"
            return {"user":{"login":"maintainer"},"body":body,"created_at":created}, "admin"
        result = verify(ledger, offline=False, fetcher=fetcher)
        return (
            result.code == 1
            and not any("stale subject digest" in x for x in result.integrity)
            and not any("subject digest is stale" in x for x in result.incomplete)
            and not any("missing current repository-maintainer scope approval" in x for x in result.incomplete)
            and any("cell" in x and "is not in a reviewed landed tranche" in x for x in result.incomplete)
        ), result.render()

    @case("stale deviation proof digest -> exit 2")
    def stale_proof(ledger: Path) -> tuple[bool, str]:
        scope = json.loads((ledger / "scope.json").read_text())
        data = json.loads((ledger / "results.json").read_text())
        cell = next(cell for cell in scope["cells"] if cell["expected_disposition"] == "safety-deviation-candidate")
        result_row = next(row for row in data["results"] if row["cell"] == cell["cell_id"])
        result_row["achieved_evidence"] = [{"id":"e1","kind":cell["expected_evidence"][0],"platform":cell["platform"],"anchor":"self-test","digest":"a"*64}]
        result_row["deviation"] = "dev-1"
        data["deviations"] = [{
            "id":"dev-1","cell":cell["cell_id"],"behavior_difference":"difference","safety_risk":"risk",
            "threat_model":"threat","invariant":"invariant","brew_anchor":"brew","zapbrew_anchor":"zapbrew",
            "reproduction":"repro","alternatives_rejected":"alternatives","evidence_refs":["e1"],
            "proof_digest":"0"*64,"created_at":"2026-01-01T00:00:00Z",
        }]
        write_json(ledger / "results.json", data)
        result = verify(ledger, offline=True)
        return result.code == 2 and any("stale proof digest" in x for x in result.integrity), result.render()

    @case("Linux native proof on macOS cell -> exit 2")
    def wrong_platform_native(ledger: Path) -> tuple[bool, str]:
        scope = json.loads((ledger / "scope.json").read_text())
        data = json.loads((ledger / "results.json").read_text())
        cell = next(
            cell for cell in scope["cells"]
            if cell["platform"] == "macos" and cell["effect"] in MUTATION_EFFECTS
        )
        result_row = next(row for row in data["results"] if row["cell"] == cell["cell_id"])
        result_row["achieved_evidence"] = [
            {"id":"e1","kind":"process-integration","platform":"linux","anchor":"self-test","digest":"a"*64},
            {"id":"e2","kind":"unit-fixture-io","platform":"any","anchor":"self-test","digest":"b"*64},
            {"id":"e3","kind":"command-construction","platform":"any","anchor":"self-test","digest":"c"*64},
            {"id":"e4","kind":"cross-compile","platform":"any","anchor":"self-test","digest":"d"*64},
        ]
        result_row["failure_evidence"] = {"status":"proved","evidence_refs":["e1"]}
        write_json(ledger / "results.json", data)
        result = verify(ledger, offline=True)
        return (
            result.code == 2
            and any("native proof platform linux" in x and "cell" in x for x in result.integrity)
        ), result.render()

    @case("product-decision-pending cell with no decision ref -> exit 2")
    def orphan_pending_cell(ledger: Path) -> tuple[bool, str]:
        scope = json.loads((ledger / "scope.json").read_text())
        cell = next(cell for cell in scope["cells"] if cell["expected_disposition"] == "product-decision-pending")
        cell["decision_refs"] = []
        write_json(ledger / "scope.json", scope)
        result = verify(ledger, offline=True)
        return result.code == 2 and any("product-decision-pending with no controlling decision" in x for x in result.integrity), result.render()

    @case("perf unit cross-platform attribution -> exit 2")
    def perf_cross_platform(ledger: Path) -> tuple[bool, str]:
        scope = json.loads((ledger / "scope.json").read_text())
        data = json.loads((ledger / "results.json").read_text())
        data["workloads"] = [{
            "id":"w1","fixture_digest":"a"*64,"source_digest":"d"*64,
            "platform":"linux","arch":"x86_64",
            "filesystem_stage":"warm","filesystem_stage_digest":"e"*64,
            "toolchain_digest":"b"*64,"build_digest":"c"*64,
            "warmups":3,"samples":[2.0]*10,
            "install_loop_samples":[2.0]*10,"sample_artifact_digest":"f"*64,
        }]
        data["perf_units"] = [{
            "id":"p1","workload":"w1","samples":[0.5]*10,
            "floor":{"expression":"calls * cost","variables":{"calls":1,"cost":0.1},"dimension":"seconds"},
            "experiment":{"pre_samples":[2.0]*10,"post_samples":[1.99]*10,"action":"revert"},
            "input_scaling":[],"cell_refs":[next(cell for cell in scope["cells"] if cell["platform"] == "macos")["cell_id"]],
            "classification":{"grade":"hot"},
        }]
        write_json(ledger / "results.json", data)
        result = verify(ledger, offline=True)
        return result.code == 2 and any("cannot attribute to macos cell" in x for x in result.integrity), result.render()

    def add_workload(ledger: Path, samples: list[float], warmups: int = 3) -> dict[str, Any]:
        data = json.loads((ledger / "results.json").read_text())
        data["workloads"] = [{
            "id":"w1","fixture_digest":"a"*64,"source_digest":"d"*64,
            "platform":"linux","arch":"x86_64",
            "filesystem_stage":"warm","filesystem_stage_digest":"e"*64,
            "toolchain_digest":"b"*64,"build_digest":"c"*64,
            "warmups":warmups,"samples":samples,
            "install_loop_samples":samples,"sample_artifact_digest":"f"*64,
        }]
        return data

    def build_fixed_unit(
        data: dict[str, Any],
        baseline_samples: list[float],
        probe_samples: list[float],
        signed_delta: float,
        ratio: float,
        *,
        cell_refs: list[str] | None = None,
        basis: str = "stage-disabled",
        probe_source_digest: str = "9"*64,
        review_anchor: str = "W3 review",
        review_digest: str = "8"*64,
    ) -> dict[str, Any]:
        baseline = {
            "id":"w2-journal-post","fixture_digest":"a"*64,"source_digest":"d"*64,
            "platform":"linux","arch":"x86_64",
            "filesystem_stage":"warm","filesystem_stage_digest":"e"*64,
            "toolchain_digest":"b"*64,"build_digest":"c"*64,
            "warmups":3,"samples":baseline_samples,
            "install_loop_samples":baseline_samples,"sample_artifact_digest":"f"*64,
        }
        probe = {
            "id":"w2-journal-disabled","fixture_digest":"a"*64,"source_digest":"d"*64,
            "platform":"linux","arch":"x86_64",
            "filesystem_stage":"warm","filesystem_stage_digest":"e"*64,
            "toolchain_digest":"b"*64,"build_digest":"d"*64,
            "warmups":3,"samples":probe_samples,
            "install_loop_samples":probe_samples,"sample_artifact_digest":"7"*64,
        }
        data["workloads"] = [baseline, probe]
        data["perf_units"] = [{
            "id":"p1","workload":"w2-journal-post","samples":[],
            "floor":None,
            "experiment":None,
            "input_scaling":[],
            "cell_refs":cell_refs or [],
            "classification":{
                "grade":"fixed",
                "basis":basis,
                "baseline_workload":"w2-journal-post",
                "probe_workload":"w2-journal-disabled",
                "probe_source_digest":probe_source_digest,
                "signed_median_delta_seconds":signed_delta,
                "baseline_over_probe_ratio":ratio,
                "review_evidence":{"anchor":review_anchor,"digest":review_digest},
            },
        }]
        return data

    @case("undersized performance samples -> exit 2")
    def undersized_samples(ledger: Path) -> tuple[bool, str]:
        data = add_workload(ledger, [1.1] * 9)
        write_json(ledger / "results.json", data)
        result = verify(ledger, offline=True)
        return result.code == 2 and any("undersized" in x for x in result.integrity), result.render()

    @case("noisy performance samples -> exit 2")
    def noisy_samples(ledger: Path) -> tuple[bool, str]:
        data = add_workload(ledger, [1.0,2.0,1.0,2.0,1.0,2.0,1.0,2.0,1.0,2.0])
        write_json(ledger / "results.json", data)
        result = verify(ledger, offline=True)
        return result.code == 2 and any("noisy" in x for x in result.integrity), result.render()

    @case("right-skewed median dispersion -> exit 2")
    def right_skewed_median_dispersion(ledger: Path) -> tuple[bool, str]:
        data = add_workload(ledger, [1.0] * 8 + [1.385, 1.586])
        write_json(ledger / "results.json", data)
        result = verify(ledger, offline=True)
        return (
            result.code == 2
            and any("noisy" in item and "median" in item for item in result.integrity)
        ), result.render()

    @case("hot above-floor no-approval -> exit 1")
    def hot_no_approval(ledger: Path) -> tuple[bool, str]:
        scope = json.loads((ledger / "scope.json").read_text())
        data = add_workload(ledger, [2.0] * 10)
        data["perf_units"] = [{
            "id":"p1","workload":"w1","samples":[0.5]*10,
            "floor":{"expression":"calls * cost","variables":{"calls":1,"cost":0.1},"dimension":"seconds"},
            "experiment":{"pre_samples":[2.0]*10,"post_samples":[1.99]*10,"action":"revert"},
            "input_scaling":[],"cell_refs":[scope["cells"][0]["cell_id"]],
            "classification":{"grade":"hot"},
        }]
        write_json(ledger / "results.json", data)
        result = verify(ledger, offline=True)
        return result.code == 1 and any(
            ("hot above-floor unit p1 remains incomplete after no-win revert" in x
             or "hot performance unit p1" in x)
            for x in result.incomplete
        ), result.render()

    @case("inflated floor without approval -> exit 1")
    def inflated_floor_without_approval(ledger: Path) -> tuple[bool, str]:
        scope = json.loads((ledger / "scope.json").read_text())
        data = add_workload(ledger, [2.0] * 10)
        data["perf_units"] = [{
            "id":"p1","workload":"w1","samples":[0.5]*10,
            "floor":{"expression":"calls * cost","variables":{"calls":1,"cost":0.4},"dimension":"seconds"},
            "experiment":None,
            "input_scaling":[],"cell_refs":[scope["cells"][0]["cell_id"]],
            "classification":{"grade":"at-floor"},
        }]
        write_json(ledger / "results.json", data)
        result = verify(ledger, offline=True)
        marker = "hot performance unit p1 lacks authenticated performance-floor approval"
        return (
            result.code == 1
            and marker in result.incomplete
            and not any(">2x" in item for item in result.incomplete)
        ), result.render()

    @case("current authenticated floor approval removes only missing-floor marker")
    def authenticated_floor_approval(ledger: Path) -> tuple[bool, str]:
        scope = json.loads((ledger / "scope.json").read_text())
        data = add_workload(ledger, [2.0] * 10)
        data["perf_units"] = [{
            "id":"p1","workload":"w1","samples":[0.5]*10,
            "floor":{"expression":"calls * cost","variables":{"calls":1,"cost":0.4},"dimension":"seconds"},
            "experiment":None,
            "input_scaling":[],"cell_refs":[scope["cells"][0]["cell_id"]],
            "classification":{"grade":"at-floor"},
        }]
        write_json(ledger / "results.json", data)
        without_approval = verify(ledger, offline=True)
        floor_digest = digest(data["perf_units"][0]["floor"])
        body = canonical_approval_line("performance-floor", "p1", floor_digest)
        event = {
            "event_id":"approval-1","kind":"performance-floor","subject_id":"p1",
            "subject_digest":floor_digest,"homebrew_commit":HOMEBREW_COMMIT,"repository":REPOSITORY,
            "actor":"maintainer",
            "object_url":f"https://api.github.com/repos/{REPOSITORY}/issues/comments/1",
            "object_body_digest":text_digest(body),"event_time":"2026-01-01T00:01:00Z",
        }
        (ledger / "approvals.jsonl").write_text(json.dumps(event)+"\n")
        def fetcher(url: str, actor: str, token: str) -> tuple[dict[str, Any], str]:
            return {"user":{"login":"maintainer"},"body":body,"created_at":"2026-01-01T00:00:00Z"}, "admin"
        with_approval = verify(ledger, offline=False, fetcher=fetcher)
        marker = "hot performance unit p1 lacks authenticated performance-floor approval"
        expected_incomplete = [item for item in without_approval.incomplete if item != marker]
        return (
            without_approval.code == 1
            and marker in without_approval.incomplete
            and with_approval.code == 1
            and marker not in with_approval.incomplete
            and with_approval.incomplete == expected_incomplete
        ), with_approval.render()

    @case("hot above-floor reclassified by authenticated approval -> note")
    def hot_reclassified(ledger: Path) -> tuple[bool, str]:
        scope = json.loads((ledger / "scope.json").read_text())
        data = add_workload(ledger, [2.0] * 10)
        data["perf_units"] = [{
            "id":"p1","workload":"w1","samples":[0.5]*10,
            "floor":{"expression":"calls * cost","variables":{"calls":1,"cost":0.1},"dimension":"seconds"},
            "experiment":{"pre_samples":[2.0]*10,"post_samples":[1.99]*10,"action":"revert"},
            "input_scaling":[],"cell_refs":[scope["cells"][0]["cell_id"]],
            "classification":{"grade":"hot"},
        }]
        write_json(ledger / "results.json", data)
        perf_digest = digest(
            {
                "workload": data["perf_units"][0]["workload"],
                "workload_record": data["workloads"][0],
                "samples": data["perf_units"][0]["samples"],
                "input_scaling": data["perf_units"][0]["input_scaling"],
            }
        )
        body = canonical_approval_line("hot-reclassification", "p1", perf_digest)
        event = {
            "event_id":"approval-1",
            "kind":"hot-reclassification",
            "subject_id":"p1",
            "subject_digest":perf_digest,
            "homebrew_commit":HOMEBREW_COMMIT,
            "repository":REPOSITORY,
            "actor":"maintainer",
            "object_url":f"https://api.github.com/repos/{REPOSITORY}/issues/comments/1",
            "object_body_digest":text_digest(body),
            "event_time":"2026-01-01T00:01:00Z",
        }
        (ledger / "approvals.jsonl").write_text(json.dumps(event)+"\n")
        def fetcher(url: str, actor: str, token: str) -> tuple[dict[str, Any], str]:
            return {"user":{"login":"maintainer"},"body":body,"created_at":"2026-01-01T00:00:00Z"}, "admin"
        result = verify(ledger, offline=False, fetcher=fetcher)
        return (
            result.code == 1
            and not any("hot performance unit p1" in x for x in result.incomplete)
            and not any("hot above-floor unit p1" in x for x in result.incomplete)
            and any("reclassified cold" in x for x in result.notes)
            and any("cell" in x and "is not in a reviewed landed tranche" in x for x in result.incomplete)
        ), result.render()

    @case("workload mutation stales hot reclassification approval -> exit 1")
    def stale_hot_reclassification_workload(ledger: Path) -> tuple[bool, str]:
        scope = json.loads((ledger / "scope.json").read_text())
        data = add_workload(ledger, [2.0] * 10)
        data["perf_units"] = [{
            "id":"p1","workload":"w1","samples":[0.5]*10,
            "floor":{"expression":"calls * cost","variables":{"calls":1,"cost":0.1},"dimension":"seconds"},
            "experiment":{"pre_samples":[2.0]*10,"post_samples":[1.99]*10,"action":"revert"},
            "input_scaling":[],"cell_refs":[scope["cells"][0]["cell_id"]],
            "classification":{"grade":"hot"},
        }]
        write_json(ledger / "results.json", data)
        reclassification_digest = digest({
            "workload": data["perf_units"][0]["workload"],
            "workload_record": data["workloads"][0],
            "samples": data["perf_units"][0]["samples"],
            "input_scaling": data["perf_units"][0]["input_scaling"],
        })
        body = canonical_approval_line("hot-reclassification", "p1", reclassification_digest)
        event = {
            "event_id":"approval-1","kind":"hot-reclassification","subject_id":"p1",
            "subject_digest":reclassification_digest,"homebrew_commit":HOMEBREW_COMMIT,"repository":REPOSITORY,
            "actor":"maintainer",
            "object_url":f"https://api.github.com/repos/{REPOSITORY}/issues/comments/1",
            "object_body_digest":text_digest(body),"event_time":"2026-01-01T00:01:00Z",
        }
        (ledger / "approvals.jsonl").write_text(json.dumps(event) + "\n")
        def fetcher(url: str, actor: str, token: str) -> tuple[dict[str, Any], str]:
            return {"user":{"login":"maintainer"},"body":body,"created_at":"2026-01-01T00:00:00Z"}, "admin"
        current = verify(ledger, offline=False, fetcher=fetcher)
        data["workloads"][0]["samples"] = [2.1] * 10
        data["workloads"][0]["install_loop_samples"] = [2.1] * 10
        write_json(ledger / "results.json", data)
        stale = verify(ledger, offline=False, fetcher=fetcher)
        return (
            any("reclassified cold" in item for item in current.notes)
            and not any("hot performance unit p1" in item for item in current.incomplete)
            and stale.code == 1
            and "approval hot-reclassification:p1 subject digest is stale" in stale.incomplete
            and not any("reclassified cold" in item for item in stale.notes)
            and any("hot performance unit p1" in item for item in stale.incomplete)
        ), stale.render()

    @case("hot above-floor no-win revert -> exit 1")
    def hot_no_win(ledger: Path) -> tuple[bool, str]:
        scope = json.loads((ledger / "scope.json").read_text())
        data = add_workload(ledger, [2.0] * 10)
        data["perf_units"] = [{
            "id":"p1","workload":"w1","samples":[0.5]*10,
            "floor":{"expression":"calls * cost","variables":{"calls":1,"cost":0.1},"dimension":"seconds"},
            "experiment":{"pre_samples":[2.0]*10,"post_samples":[1.99]*10,"action":"revert"},
            "input_scaling":[],"cell_refs":[scope["cells"][0]["cell_id"]],
            "classification":{"grade":"hot"},
        }]
        write_json(ledger / "results.json", data)
        result = verify(ledger, offline=True)
        return result.code == 1 and any("hot above-floor unit p1 remains incomplete after no-win revert" in x for x in result.incomplete), result.render()

    @case("fixed unit valid shape stays incomplete not invalid -> exit 1")
    def fixed_valid_shape(ledger: Path) -> tuple[bool, str]:
        scope = json.loads((ledger / "scope.json").read_text())
        data = json.loads((ledger / "results.json").read_text())
        data = build_fixed_unit(
            data,
            [2.33579] * 10,
            [2.36056691758] * 10,
            -0.02477691758,
            0.989503827493524,
            cell_refs=[scope["cells"][0]["cell_id"]],
        )
        write_json(ledger / "results.json", data)
        result = verify(ledger, offline=True)
        return (
            result.code == 1
            and not result.integrity
            and not any("hot performance unit p1" in x for x in result.incomplete)
            and not any("hot above-floor unit p1" in x for x in result.incomplete)
        ), result.render()

    @case("fixed unit with direct samples -> exit 2")
    def fixed_direct_samples(ledger: Path) -> tuple[bool, str]:
        scope = json.loads((ledger / "scope.json").read_text())
        data = json.loads((ledger / "results.json").read_text())
        data = build_fixed_unit(
            data, [2.33579] * 10, [2.36056691758] * 10, -0.02477691758, 0.989503827493524,
            cell_refs=[scope["cells"][0]["cell_id"]],
        )
        data["perf_units"][0]["samples"] = [0.1] * 10
        write_json(ledger / "results.json", data)
        result = verify(ledger, offline=True)
        return result.code == 2 and any("fixed unit p1 carries direct unit samples" in x for x in result.integrity), result.render()

    @case("fixed unit with non-null floor -> exit 2")
    def fixed_nonnull_floor(ledger: Path) -> tuple[bool, str]:
        scope = json.loads((ledger / "scope.json").read_text())
        data = json.loads((ledger / "results.json").read_text())
        data = build_fixed_unit(
            data, [2.33579] * 10, [2.36056691758] * 10, -0.02477691758, 0.989503827493524,
            cell_refs=[scope["cells"][0]["cell_id"]],
        )
        data["perf_units"][0]["floor"] = {"expression":"calls * cost","variables":{"calls":1,"cost":0.1},"dimension":"seconds"}
        write_json(ledger / "results.json", data)
        result = verify(ledger, offline=True)
        return result.code == 2 and any("fixed unit p1 carries a floor" in x for x in result.integrity), result.render()

    @case("fixed unit with experiment -> exit 2")
    def fixed_experiment(ledger: Path) -> tuple[bool, str]:
        scope = json.loads((ledger / "scope.json").read_text())
        data = json.loads((ledger / "results.json").read_text())
        data = build_fixed_unit(
            data, [2.33579] * 10, [2.36056691758] * 10, -0.02477691758, 0.989503827493524,
            cell_refs=[scope["cells"][0]["cell_id"]],
        )
        data["perf_units"][0]["experiment"] = {"pre_samples":[2.0]*10,"post_samples":[1.99]*10,"action":"revert"}
        write_json(ledger / "results.json", data)
        result = verify(ledger, offline=True)
        return result.code == 2 and any("fixed unit p1 carries an optimization experiment" in x for x in result.integrity), result.render()

    @case("fixed unit with scaling claim -> exit 2")
    def fixed_scaling(ledger: Path) -> tuple[bool, str]:
        scope = json.loads((ledger / "scope.json").read_text())
        data = json.loads((ledger / "results.json").read_text())
        data = build_fixed_unit(
            data, [2.33579] * 10, [2.36056691758] * 10, -0.02477691758, 0.989503827493524,
            cell_refs=[scope["cells"][0]["cell_id"]],
        )
        data["perf_units"][0]["input_scaling"] = [{"input_size":1,"fixture_digest":"h"*64,"samples":[1.0]*10}, {"input_size":2,"fixture_digest":"i"*64,"samples":[2.1]*10}]
        write_json(ledger / "results.json", data)
        result = verify(ledger, offline=True)
        return result.code == 2 and any("fixed unit p1 carries a scaling claim" in x for x in result.integrity), result.render()

    @case("fixed signed delta mismatch -> exit 2")
    def fixed_delta_mismatch(ledger: Path) -> tuple[bool, str]:
        scope = json.loads((ledger / "scope.json").read_text())
        data = json.loads((ledger / "results.json").read_text())
        data = build_fixed_unit(
            data, [2.33579] * 10, [2.36056691758] * 10, 0.0, 0.989503827493524,
            cell_refs=[scope["cells"][0]["cell_id"]],
        )
        write_json(ledger / "results.json", data)
        result = verify(ledger, offline=True)
        return result.code == 2 and any("fixed signed delta does not match measured medians" in x for x in result.integrity), result.render()

    @case("fixed ratio mismatch -> exit 2")
    def fixed_ratio_mismatch(ledger: Path) -> tuple[bool, str]:
        scope = json.loads((ledger / "scope.json").read_text())
        data = json.loads((ledger / "results.json").read_text())
        data = build_fixed_unit(
            data, [2.33579] * 10, [2.36056691758] * 10, -0.02477691758, 1.0,
            cell_refs=[scope["cells"][0]["cell_id"]],
        )
        write_json(ledger / "results.json", data)
        result = verify(ledger, offline=True)
        return result.code == 2 and any("fixed ratio mismatch" in x for x in result.integrity), result.render()

    @case("fixed grade with >=5pct disable speedup -> exit 2")
    def fixed_hot_disable(ledger: Path) -> tuple[bool, str]:
        scope = json.loads((ledger / "scope.json").read_text())
        data = json.loads((ledger / "results.json").read_text())
        data = build_fixed_unit(
            data, [2.0] * 10, [1.8] * 10, 0.2, 1.1111111111111112,
            cell_refs=[scope["cells"][0]["cell_id"]],
        )
        write_json(ledger / "results.json", data)
        result = verify(ledger, offline=True)
        return result.code == 2 and any("fixed grade rejected: disabling the stage saves >=5%" in x for x in result.integrity), result.render()

    @case("fixed stage-disabled probe same build -> exit 2")
    def fixed_same_build(ledger: Path) -> tuple[bool, str]:
        scope = json.loads((ledger / "scope.json").read_text())
        data = json.loads((ledger / "results.json").read_text())
        data = build_fixed_unit(
            data, [2.33579] * 10, [2.36056691758] * 10, -0.02477691758, 0.989503827493524,
            cell_refs=[scope["cells"][0]["cell_id"]],
        )
        data["workloads"][1]["build_digest"] = "c" * 64
        write_json(ledger / "results.json", data)
        result = verify(ledger, offline=True)
        return result.code == 2 and any("stage-disabled probe is the same build as baseline" in x for x in result.integrity), result.render()

    @case("fixed stage-disabled probe alignment break -> exit 2")
    def fixed_alignment_break(ledger: Path) -> tuple[bool, str]:
        scope = json.loads((ledger / "scope.json").read_text())
        data = json.loads((ledger / "results.json").read_text())
        data = build_fixed_unit(
            data, [2.33579] * 10, [2.36056691758] * 10, -0.02477691758, 0.989503827493524,
            cell_refs=[scope["cells"][0]["cell_id"]],
        )
        data["workloads"][1]["toolchain_digest"] = "x" * 64
        write_json(ledger / "results.json", data)
        result = verify(ledger, offline=True)
        return result.code == 2 and any("stage-disabled probe for p1 is not aligned" in x for x in result.integrity), result.render()

    @case("fixed review evidence bad digest -> exit 2")
    def fixed_review_evidence_bad_digest(ledger: Path) -> tuple[bool, str]:
        scope = json.loads((ledger / "scope.json").read_text())
        data = json.loads((ledger / "results.json").read_text())
        data = build_fixed_unit(
            data, [2.33579] * 10, [2.36056691758] * 10, -0.02477691758, 0.989503827493524,
            cell_refs=[scope["cells"][0]["cell_id"]],
            review_digest="not-a-digest",
        )
        write_json(ledger / "results.json", data)
        result = verify(ledger, offline=True)
        return result.code == 2 and any("fixed unit p1.review_evidence.digest must be sha256" in x for x in result.integrity), result.render()

    @case("workload install loop below 90pct wall -> exit 2")
    def workload_loop_below_wall(ledger: Path) -> tuple[bool, str]:
        data = add_workload(ledger, [2.0] * 10)
        data["workloads"][0]["install_loop_samples"] = [1.5] * 10
        write_json(ledger / "results.json", data)
        result = verify(ledger, offline=True)
        return result.code == 2 and any("install loop below 90% of wall time" in x for x in result.integrity), result.render()

    @case("at-floor grade contradicts cold measurement -> exit 2")
    def atfloor_cold_contradiction(ledger: Path) -> tuple[bool, str]:
        scope = json.loads((ledger / "scope.json").read_text())
        data = add_workload(ledger, [2.0] * 10)
        data["perf_units"] = [{
            "id":"p1","workload":"w1","samples":[0.05]*10,
            "floor":{"expression":"calls * cost","variables":{"calls":1,"cost":0.1},"dimension":"seconds"},
            "experiment":None,
            "input_scaling":[],"cell_refs":[scope["cells"][0]["cell_id"]],
            "classification":{"grade":"at-floor"},
        }]
        write_json(ledger / "results.json", data)
        result = verify(ledger, offline=True)
        return result.code == 2 and any("at-floor grade contradicts measurement" in x for x in result.integrity), result.render()

    @case("hot grade contradicts cold measurement -> exit 2")
    def hot_cold_contradiction(ledger: Path) -> tuple[bool, str]:
        scope = json.loads((ledger / "scope.json").read_text())
        data = add_workload(ledger, [2.0] * 10)
        data["perf_units"] = [{
            "id":"p1","workload":"w1","samples":[0.05]*10,
            "floor":{"expression":"calls * cost","variables":{"calls":1,"cost":0.1},"dimension":"seconds"},
            "experiment":{"pre_samples":[2.0]*10,"post_samples":[1.99]*10,"action":"revert"},
            "input_scaling":[],"cell_refs":[scope["cells"][0]["cell_id"]],
            "classification":{"grade":"hot"},
        }]
        write_json(ledger / "results.json", data)
        result = verify(ledger, offline=True)
        return result.code == 2 and any("hot grade contradicts a measured cold unit" in x for x in result.integrity), result.render()

    @case("performance-floor approval on fixed unit -> exit 2")
    def fixed_floor_approval_rejected(ledger: Path) -> tuple[bool, str]:
        scope = json.loads((ledger / "scope.json").read_text())
        data = json.loads((ledger / "results.json").read_text())
        data = build_fixed_unit(
            data, [2.33579] * 10, [2.36056691758] * 10, -0.02477691758, 0.989503827493524,
            cell_refs=[scope["cells"][0]["cell_id"]],
        )
        write_json(ledger / "results.json", data)
        floor_digest = digest({"expression":"calls * cost","variables":{"calls":1,"cost":0.1},"dimension":"seconds"})
        body = canonical_approval_line("performance-floor", "p1", floor_digest)
        event = {
            "event_id":"approval-1","kind":"performance-floor","subject_id":"p1",
            "subject_digest":floor_digest,"homebrew_commit":HOMEBREW_COMMIT,"repository":REPOSITORY,
            "actor":"maintainer",
            "object_url":f"https://api.github.com/repos/{REPOSITORY}/issues/comments/1",
            "object_body_digest":text_digest(body),"event_time":"2026-01-01T00:01:00Z",
        }
        (ledger / "approvals.jsonl").write_text(json.dumps(event)+"\n")
        def fetcher(url: str, actor: str, token: str) -> tuple[dict[str, Any], str]:
            return {"user":{"login":"maintainer"},"body":body,"created_at":"2026-01-01T00:00:00Z"}, "admin"
        result = verify(ledger, offline=False, fetcher=fetcher)
        return (
            result.code == 2
            and "approvals.jsonl event 1 references unknown or mismatched subject" in result.integrity
        ), result.render()

    @case("hot-reclassification approval on fixed unit -> exit 2")
    def fixed_reclass_approval_rejected(ledger: Path) -> tuple[bool, str]:
        scope = json.loads((ledger / "scope.json").read_text())
        data = json.loads((ledger / "results.json").read_text())
        data = build_fixed_unit(
            data, [2.33579] * 10, [2.36056691758] * 10, -0.02477691758, 0.989503827493524,
            cell_refs=[scope["cells"][0]["cell_id"]],
        )
        write_json(ledger / "results.json", data)
        perf_digest = digest({
            "workload": data["perf_units"][0]["workload"],
            "workload_record": data["workloads"][0],
            "samples": data["perf_units"][0]["samples"],
            "input_scaling": data["perf_units"][0]["input_scaling"],
        })
        body = canonical_approval_line("hot-reclassification", "p1", perf_digest)
        event = {
            "event_id":"approval-1","kind":"hot-reclassification","subject_id":"p1",
            "subject_digest":perf_digest,"homebrew_commit":HOMEBREW_COMMIT,"repository":REPOSITORY,
            "actor":"maintainer",
            "object_url":f"https://api.github.com/repos/{REPOSITORY}/issues/comments/1",
            "object_body_digest":text_digest(body),"event_time":"2026-01-01T00:01:00Z",
        }
        (ledger / "approvals.jsonl").write_text(json.dumps(event)+"\n")
        def fetcher(url: str, actor: str, token: str) -> tuple[dict[str, Any], str]:
            return {"user":{"login":"maintainer"},"body":body,"created_at":"2026-01-01T00:00:00Z"}, "admin"
        result = verify(ledger, offline=False, fetcher=fetcher)
        return (
            result.code == 2
            and "approvals.jsonl event 1 references unknown or mismatched subject" in result.integrity
        ), result.render()

    @case("platform-any single-host native proof stays incomplete -> exit 1")
    def single_host_native_incomplete(ledger: Path) -> tuple[bool, str]:
        scope = json.loads((ledger / "scope.json").read_text())
        data = json.loads((ledger / "results.json").read_text())
        cell = next(
            cell for cell in scope["cells"]
            if cell["platform"] == "any"
            and cell["effect"] in MUTATION_EFFECTS
            and set(cell["expected_evidence"]) & NATIVE_PROOF_KINDS
        )
        result_row = next(row for row in data["results"] if row["cell"] == cell["cell_id"])
        result_row["achieved_evidence"] = [
            {
                "id": f"self-{index}",
                "kind": kind,
                "platform": "linux" if kind in NATIVE_PROOF_KINDS else "any",
                "anchor": "self-test",
                "digest": "a" * 64,
            }
            for index, kind in enumerate(cell["expected_evidence"])
        ]
        result_row["failure_evidence"] = {"status": "proved", "evidence_refs": ["self-0"]}
        write_json(ledger / "results.json", data)
        result = verify(ledger, offline=True)
        marker = f"platform-any mutation cell {cell['cell_id']} lacks native proof on ['macos']"
        return result.code == 1 and marker in result.incomplete, result.render()

    def tranche_review_fixture(ledger: Path, *, landed: bool = False) -> dict[str, Any]:
        scope = json.loads((ledger / "scope.json").read_text())
        result_data = json.loads((ledger / "results.json").read_text())
        cells = {row["cell_id"]: row for row in scope["cells"]}
        results = {row["cell"]: row for row in result_data["results"]}
        decisions = {row["id"]: row for row in scope["decisions"]}
        deviations = {row["id"]: row for row in result_data["deviations"]}
        workloads = {row["id"]: row for row in result_data["workloads"]}
        perf_units = {row["id"]: row for row in result_data["perf_units"]}
        cell_id = scope["cells"][0]["cell_id"]
        commit = "a" * 40
        events = [
            {"event_id":"t1","tranche_id":"tranche-1","from":"proposed","to":"scoped","event_time":"2026-01-01T00:00:00Z","cells":[cell_id]},
            {"event_id":"t2","tranche_id":"tranche-1","from":"scoped","to":"building","event_time":"2026-01-01T00:01:00Z"},
            {"event_id":"t3","tranche_id":"tranche-1","from":"building","to":"evidence-pending","event_time":"2026-01-01T00:02:00Z"},
            {"event_id":"t4","tranche_id":"tranche-1","from":"evidence-pending","to":"review-pending","event_time":"2026-01-01T00:03:00Z","repository_commit":commit},
            {"event_id":"t5","tranche_id":"tranche-1","from":"review-pending","to":"approved","event_time":"2026-01-01T00:05:00Z","review_ref":"review-1"},
        ]
        if landed:
            events.append({"event_id":"t6","tranche_id":"tranche-1","from":"approved","to":"landed","event_time":"2026-01-01T00:06:00Z"})
        replay_report = Report()
        states, _ = replay_tranches(events, cells, decisions, workloads, replay_report)
        if replay_report.integrity:
            raise AssertionError(replay_report.render())
        state = states["tranche-1"]
        subject = tranche_review_subject(
            "tranche-1", state, cells, results, deviations, perf_units, workloads, decisions
        )
        if subject is None:
            raise AssertionError("self-test tranche-review subject could not be built")
        subject_hash = digest(subject)
        body = canonical_tranche_review_line("tranche-1", "t4", commit, subject_hash)
        approval = {
            "event_id":"review-1", "kind":"tranche-review", "subject_id":"tranche-1",
            "subject_digest":subject_hash, "homebrew_commit":HOMEBREW_COMMIT,
            "repository":REPOSITORY, "actor":"reviewer",
            "object_url":f"https://api.github.com/repos/{REPOSITORY}/pulls/7/reviews/9",
            "object_body_digest":text_digest(body), "event_time":"2026-01-01T00:05:00Z",
        }
        review = {
            "id":9, "pull_request_url":f"https://api.github.com/repos/{REPOSITORY}/pulls/7",
            "user":{"login":"reviewer"}, "body":body, "state":"APPROVED",
            "commit_id":commit, "submitted_at":"2026-01-01T00:04:00Z",
        }
        pull = {
            "number": 7,
            "user": {"login": "author"},
            "base": {"ref": "main"},
            "head": {"sha": commit},
        }
        return locals()

    def authenticate_tranche_review(
        fixture: dict[str, Any], *, offline: bool = False, permission: str = "maintain",
        review_changes: dict[str, Any] | None = None, pull_changes: dict[str, Any] | None = None,
        approval_changes: dict[str, Any] | None = None, unavailable: bool = False,
    ) -> tuple[Report, dict[str, TrancheState], dict[str, dict[str, Any]]]:
        review = copy.deepcopy(fixture["review"])
        pull = copy.deepcopy(fixture["pull"])
        approval = copy.deepcopy(fixture["approval"])
        review.update(review_changes or {})
        pull.update(pull_changes or {})
        approval.update(approval_changes or {})
        report = Report()
        states = copy.deepcopy(fixture["states"])

        def fetcher(url: str, actor: str, token: str) -> tuple[dict[str, Any], str, dict[str, Any]]:
            if unavailable:
                raise OSError("offline self-test")
            return review, permission, pull

        approved, event_ids, review_events, authenticated = validate_approvals(
            [approval], fixture["scope"], fixture["decisions"], fixture["deviations"],
            fixture["workloads"], fixture["perf_units"], offline, report, fetcher=fetcher,
            cells=fixture["cells"], results=fixture["results"], states=states,
        )
        bind_tranche_reviews(states, event_ids, review_events, authenticated, report)
        return report, states, review_events

    def review_failure_case(
        name: str, marker: str, *, expected_code: int = 1, permission: str = "maintain",
        review_changes: dict[str, Any] | None = None, pull_changes: dict[str, Any] | None = None,
        approval_changes: dict[str, Any] | None = None, offline: bool = False, unavailable: bool = False,
        approved_at: str | None = None,
    ) -> None:
        @case(name)
        def generated(ledger: Path) -> tuple[bool, str]:
            fixture = tranche_review_fixture(ledger)
            if approved_at is not None:
                fixture["states"]["tranche-1"].approved_at = parse_time_soft(approved_at)
            report, states, _ = authenticate_tranche_review(
                fixture, offline=offline, permission=permission, review_changes=review_changes,
                pull_changes=pull_changes, approval_changes=approval_changes, unavailable=unavailable,
            )
            messages = report.integrity if expected_code == 2 else report.incomplete
            return report.code == expected_code and any(marker in item for item in messages) and not states["tranche-1"].reviewed, report.render()

    review_failure_case("valid tranche review offline remains unreviewed -> exit 1", "requires online authority proof", offline=True)
    review_failure_case("tranche review fetch unavailable remains unreviewed -> exit 1", "online authority proof unavailable", unavailable=True)
    review_failure_case("noncanonical tranche-review URL -> exit 2", "canonical PR review API URL", expected_code=2, approval_changes={"object_url":f"https://api.github.com/repos/{REPOSITORY}/pulls/7/reviews/9/"})
    review_failure_case("review object ID mismatch -> exit 1", "review URL/object identity mismatch", review_changes={"id":10})
    review_failure_case("review pull-request URL mismatch -> exit 1", "review URL/object identity mismatch", review_changes={"pull_request_url":f"https://api.github.com/repos/{REPOSITORY}/pulls/8"})
    review_failure_case("review actor mismatch -> exit 1", "object actor mismatch", review_changes={"user":{"login":"other"}})
    review_failure_case("pull-request author self-review -> exit 1", "reviewer is the pull request author", pull_changes={"user":{"login":"reviewer"}})
    review_failure_case("reviewer permission insufficient -> exit 1", "actor lacks current maintain/admin permission", permission="write")
    review_failure_case("pull-request base mismatch -> exit 1", "pull request base is not main", pull_changes={"base":{"ref":"release"}})
    review_failure_case("pull-request head mismatch -> exit 1", "pull request head does not match checkpoint", pull_changes={"head":{"sha":"b" * 40}})
    review_failure_case("review state not APPROVED -> exit 1", "review state is not APPROVED", review_changes={"state":"CHANGES_REQUESTED"})
    review_failure_case("review commit mismatch -> exit 1", "reviewed commit does not match checkpoint", review_changes={"commit_id":"b" * 40})
    review_failure_case("review body is not canonical -> exit 1", "review body is not the exact canonical tranche-review line", review_changes={"body":"not canonical"}, approval_changes={"object_body_digest":text_digest("not canonical")})
    review_failure_case("review body digest mismatch -> exit 1", "object body digest is stale", review_changes={"body":"tampered"})
    review_failure_case("review submitted before checkpoint -> exit 2", "submitted before its checkpoint", expected_code=2, review_changes={"submitted_at":"2026-01-01T00:02:00Z"})
    review_failure_case("approval event precedes submitted review -> exit 2", "submitted after its approval transition", expected_code=2, review_changes={"submitted_at":"2026-01-01T00:05:01Z"}, approved_at="2026-01-01T00:06:00Z")

    @case("legacy forged review_ref names no approval -> exit 2")
    def forged_review_ref(ledger: Path) -> tuple[bool, str]:
        fixture = tranche_review_fixture(ledger)
        report = Report()
        bind_tranche_reviews(fixture["states"], set(), {}, set(), report)
        return report.code == 2 and any("review_ref names no approval event" in item for item in report.integrity), report.render()

    @case("review checkpoint without repository commit -> exit 2")
    def checkpoint_without_commit(ledger: Path) -> tuple[bool, str]:
        fixture = tranche_review_fixture(ledger)
        events = copy.deepcopy(fixture["events"]); events[3].pop("repository_commit")
        report = Report(); replay_tranches(events, fixture["cells"], fixture["decisions"], fixture["workloads"], report)
        return report.code == 2 and any("review checkpoint requires repository_commit" in item for item in report.integrity), report.render()

    def review_ref_binding_case(name: str, marker: str, mutate: Callable[[dict[str, Any]], None]) -> None:
        @case(name)
        def generated(ledger: Path) -> tuple[bool, str]:
            fixture = tranche_review_fixture(ledger); event = copy.deepcopy(fixture["approval"]); mutate(event)
            report = Report(); bind_tranche_reviews(fixture["states"], {event["event_id"]}, {event["event_id"]: event} if event.get("kind") == "tranche-review" else {}, {event["event_id"]}, report)
            return report.code == 2 and any(marker in item for item in report.integrity), report.render()
    review_ref_binding_case("review_ref names non-review approval -> exit 2", "non-tranche-review approval", lambda event: event.update(kind="scope"))
    review_ref_binding_case("review_ref names another tranche review -> exit 2", "another tranche's review", lambda event: event.update(subject_id="tranche-2"))
    review_ref_binding_case("review_ref names late review event -> exit 2", "names a late approval event", lambda event: event.update(event_time="2026-01-01T00:05:01Z"))

    def closure_mutation_case(name: str, target: str) -> None:
        @case(name)
        def generated(ledger: Path) -> tuple[bool, str]:
            fixture = tranche_review_fixture(ledger)
            if target == "scope": fixture["cells"][next(iter(fixture["states"]["tranche-1"].cells))]["behavior"] += " changed"
            elif target == "result": fixture["results"][next(iter(fixture["states"]["tranche-1"].cells))]["finding"] = "missing"
            else:
                collection = fixture[target]
                collection[f"{target}-1"] = {"changed": True}
                if target == "deviations":
                    fixture["results"][next(iter(fixture["states"]["tranche-1"].cells))]["deviation"] = f"{target}-1"
                elif target == "perf_units":
                    collection[f"{target}-1"]["workload"] = "perf-workload-1"
                    fixture["workloads"]["perf-workload-1"] = {
                        "id": "perf-workload-1",
                        "fixture_digest": "a" * 64,
                        "source_digest": "d" * 64,
                        "platform": "linux",
                        "arch": "x86_64",
                        "filesystem_stage": "warm",
                        "filesystem_stage_digest": "e" * 64,
                        "toolchain_digest": "b" * 64,
                        "build_digest": "c" * 64,
                        "warmups": 3,
                        "samples": [2.0] * 10,
                        "install_loop_samples": [2.0] * 10,
                        "sample_artifact_digest": "f" * 64,
                    }
                    collection[f"{target}-1"] = {
                        "id": f"{target}-1",
                        "workload": "perf-workload-1",
                        "samples": [0.5] * 10,
                        "floor": {"expression": "calls * cost", "variables": {"calls": 1, "cost": 0.1}, "dimension": "seconds"},
                        "experiment": {"pre_samples": [2.0] * 10, "post_samples": [1.99] * 10, "action": "revert"},
                        "input_scaling": [],
                        "cell_refs": [next(iter(fixture["states"]["tranche-1"].cells))],
                        "classification": {"grade": "hot"},
                    }
                    fixture["results"][next(iter(fixture["states"]["tranche-1"].cells))]["perf_refs"] = [f"{target}-1"]
                elif target == "workloads":
                    fixture["states"]["tranche-1"].context_workloads.add(f"{target}-1")
                elif target == "decisions":
                    fixture["states"]["tranche-1"].context_decisions.add(f"{target}-1")
            report, states, _ = authenticate_tranche_review(fixture)
            return report.code == 1 and any("subject digest is stale" in item for item in report.incomplete) and not states["tranche-1"].reviewed, report.render()
    for mutation_target in ("scope", "result", "deviations", "perf_units", "workloads", "decisions"):
        closure_mutation_case(f"{mutation_target} closure mutation stales tranche review -> exit 1", mutation_target)

    @case("unrelated tranche append preserves current review")
    def unrelated_append(ledger: Path) -> tuple[bool, str]:
        fixture = tranche_review_fixture(ledger)
        unrelated_cell = fixture["scope"]["cells"][1]["cell_id"]
        fixture["decisions"]["D-unrelated"] = {"id": "D-unrelated", "status": "pending"}
        fixture["workloads"]["W-unrelated"] = {
            "id": "W-unrelated",
            "fixture_digest": "a" * 64,
            "source_digest": "d" * 64,
            "platform": "linux",
            "arch": "x86_64",
            "filesystem_stage": "warm",
            "filesystem_stage_digest": "e" * 64,
            "toolchain_digest": "b" * 64,
            "build_digest": "c" * 64,
            "warmups": 3,
            "samples": [2.0] * 10,
            "install_loop_samples": [2.0] * 10,
            "sample_artifact_digest": "f" * 64,
        }
        fixture["deviations"]["DEV-unrelated"] = {"id": "DEV-unrelated"}
        fixture["perf_units"]["PERF-unrelated"] = {
            "id": "PERF-unrelated",
            "workload": "W-unrelated",
            "samples": [0.5] * 10,
            "floor": {"expression": "calls * cost", "variables": {"calls": 1, "cost": 0.1}, "dimension": "seconds"},
            "experiment": {"pre_samples": [2.0] * 10, "post_samples": [1.99] * 10, "action": "revert"},
            "input_scaling": [],
            "cell_refs": [unrelated_cell],
            "classification": {"grade": "hot"},
        }
        unrelated_events = [
            {"event_id":"u1","tranche_id":"tranche-2","from":"proposed","to":"scoped","event_time":"2026-01-01T00:00:00Z","cells":[unrelated_cell]},
            {"event_id":"u2","tranche_id":"tranche-2","from":"scoped","to":"building","event_time":"2026-01-01T00:01:00Z"},
            {"event_id":"u3","tranche_id":"tranche-2","from":"building","to":"evidence-pending","event_time":"2026-01-01T00:02:00Z"},
            {"event_id":"u4","tranche_id":"tranche-2","from":"evidence-pending","to":"review-pending","event_time":"2026-01-01T00:03:00Z","repository_commit":"b"*40,"decision_refs":["D-unrelated"],"workload_refs":["W-unrelated"]},
        ]
        replay_report = Report()
        fixture["states"], _ = replay_tranches(
            fixture["events"] + unrelated_events,
            fixture["cells"],
            fixture["decisions"],
            fixture["workloads"],
            replay_report,
        )
        if replay_report.integrity:
            return False, replay_report.render()
        report, states, _ = authenticate_tranche_review(fixture)
        return report.code == 0 and states["tranche-1"].reviewed, report.render()

    @case("non-monotonic tranche event time -> exit 2")
    def non_monotonic_time(ledger: Path) -> tuple[bool, str]:
        fixture = tranche_review_fixture(ledger); events = copy.deepcopy(fixture["events"]); events[1]["event_time"] = events[0]["event_time"]
        report = Report(); replay_tranches(events, fixture["cells"], fixture["decisions"], fixture["workloads"], report)
        return report.code == 2 and any("time must be strictly increasing" in item for item in report.integrity), report.render()

    @case("duplicate tranche and cross-log event IDs -> exit 2")
    def duplicate_tranche_event_id(ledger: Path) -> tuple[bool, str]:
        fixture = tranche_review_fixture(ledger)
        events = copy.deepcopy(fixture["events"])
        events[1]["event_id"] = events[0]["event_id"]
        replay_report = Report()
        replay_tranches(events, fixture["cells"], fixture["decisions"], fixture["workloads"], replay_report)
        within_log_rejected = replay_report.code == 2 and any(
            "repeats tranche event_id" in item for item in replay_report.integrity
        )
        cross_log_events = copy.deepcopy(fixture["events"])
        cross_log_events[0]["event_id"] = fixture["approval"]["event_id"]
        (ledger / "tranches.jsonl").write_text(
            "".join(json.dumps(row) + "\n" for row in cross_log_events)
        )
        (ledger / "approvals.jsonl").write_text(json.dumps(fixture["approval"]) + "\n")
        cross_log_report = verify(ledger, offline=True)
        cross_log_rejected = cross_log_report.code == 2 and any(
            "globally unique across approval and tranche logs" in item
            for item in cross_log_report.integrity
        )
        return within_log_rejected and cross_log_rejected, cross_log_report.render()

    @case("empty logs bootstrap remains exit 1")
    def empty_logs_bootstrap(ledger: Path) -> tuple[bool, str]:
        (ledger / "tranches.jsonl").write_text(""); (ledger / "approvals.jsonl").write_text("")
        report = verify(ledger, offline=True)
        return report.code == 1 and not report.integrity and any("not in a reviewed landed tranche" in item for item in report.incomplete), report.render()

    def landed_fetcher(ledger: Path, fixture: dict[str, Any], overrides: dict[str, Any] | None = None) -> Callable[[str, str, str], tuple[Any, ...]]:
        overrides = overrides or {}
        def fetch(url: str, actor: str, token: str) -> tuple[Any, ...]:
            for needle, value in overrides.items():
                if needle in url:
                    if isinstance(value, Exception): raise value
                    return value if isinstance(value, tuple) else (value, "maintain", {})
            if "/reviews/" in url: return fixture["review"], "maintain", fixture["pull"]
            if "/compare/" in url: return {"status":"ahead"}, "maintain", {}
            if "/contents/" in url:
                name = url.split("/contents/.outline/ledger/", 1)[1].split("?", 1)[0]
                content = (ledger / name).read_bytes()
                encoded = base64.b64encode(content).decode()
                wrapped = "\n".join(encoded[index:index + 60] for index in range(0, len(encoded), 60))
                return {"encoding":"base64","content":wrapped}, "maintain", {}
            return {"default_branch":"main"}, "maintain", {}
        return fetch

    def landed_case(name: str, marker: str, overrides: dict[str, Any], *, expected_code: int = 2) -> None:
        @case(name)
        def generated(ledger: Path) -> tuple[bool, str]:
            fixture = tranche_review_fixture(ledger, landed=True)
            (ledger / "tranches.jsonl").write_text("".join(json.dumps(row)+"\n" for row in fixture["events"]))
            report, states, review_events = authenticate_tranche_review(fixture)
            if report.code != 0: return False, report.render()
            validate_landed_checkpoints(ledger, states, review_events, report, landed_fetcher(ledger, fixture, overrides))
            messages = report.integrity if expected_code == 2 else report.incomplete
            return report.code == expected_code and any(marker in item for item in messages), report.render()
    landed_case("landed checkpoint unreachable from default branch -> exit 2", "not on the default branch", {"/compare/":{"status":"diverged"}})
    landed_case("landed repository fetch unavailable -> exit 1", "repository is unreachable", {f"/repos/{REPOSITORY}":OSError("offline")}, expected_code=1)
    landed_case("landed compare fetch unavailable -> exit 1", "checkpoint comparison is unreachable", {"/compare/":OSError("offline")}, expected_code=1)
    landed_case("landed comparison missing status -> exit 1", "comparison has no reachable status", {"/compare/":{}}, expected_code=1)
    landed_case("landed repository default branch mismatch -> exit 2", "repository default_branch is not main", {f"/repos/{REPOSITORY}":{"default_branch":"release"}}, expected_code=2)
    landed_case("landed committed file malformed base64 -> exit 2", "content is malformed", {"scope.json":{"encoding":"base64","content":"%%%"}})
    landed_case("landed committed file malformed JSON -> exit 2", "content is malformed", {"scope.json":{"encoding":"base64","content":base64.b64encode(b"{").decode()}})

    @case("exact reachable committed snapshot has no landed-review error")
    def exact_landed_snapshot(ledger: Path) -> tuple[bool, str]:
        fixture = tranche_review_fixture(ledger, landed=True); (ledger / "tranches.jsonl").write_text("".join(json.dumps(row)+"\n" for row in fixture["events"]))
        extra = ledger.parent.parent / "crates" / "later" / "src" / "added.rs"
        extra.parent.mkdir(parents=True)
        extra.write_text("// later live-tree addition\n")
        report, states, review_events = authenticate_tranche_review(fixture)
        validate_landed_checkpoints(ledger, states, review_events, report, landed_fetcher(ledger, fixture))
        landed_errors = [item for item in report.integrity + report.incomplete if "tranche tranche-1" in item and ("checkpoint" in item or "committed" in item or "repository" in item)]
        return not landed_errors, report.render()

    @case("reverted tranche replaced by reviewed landed tranche -> exit 1")
    def reverted_releases_ownership(ledger: Path) -> tuple[bool, str]:
        fixture = tranche_review_fixture(ledger)
        cell = next(iter(fixture["states"]["tranche-1"].cells))
        commit = "b" * 40
        events = [
            {"event_id":"r1","tranche_id":"tranche-1","from":"proposed","to":"scoped","event_time":"2026-01-01T00:00:00Z","cells":[cell]},
            {"event_id":"r2","tranche_id":"tranche-1","from":"scoped","to":"building","event_time":"2026-01-01T00:01:00Z"},
            {"event_id":"r3","tranche_id":"tranche-1","from":"building","to":"evidence-pending","event_time":"2026-01-01T00:02:00Z"},
            {"event_id":"r4","tranche_id":"tranche-1","from":"evidence-pending","to":"review-pending","event_time":"2026-01-01T00:03:00Z","repository_commit":"a"*40},
            {"event_id":"r5","tranche_id":"tranche-1","from":"review-pending","to":"approved","event_time":"2026-01-01T00:04:00Z","review_ref":"superseded-review"},
            {"event_id":"r6","tranche_id":"tranche-1","from":"approved","to":"reverted","event_time":"2026-01-01T00:05:00Z"},
            {"event_id":"n1","tranche_id":"tranche-2","from":"proposed","to":"scoped","event_time":"2026-01-01T00:00:00Z","cells":[cell]},
            {"event_id":"n2","tranche_id":"tranche-2","from":"scoped","to":"building","event_time":"2026-01-01T00:01:00Z"},
            {"event_id":"n3","tranche_id":"tranche-2","from":"building","to":"evidence-pending","event_time":"2026-01-01T00:02:00Z"},
            {"event_id":"n4","tranche_id":"tranche-2","from":"evidence-pending","to":"review-pending","event_time":"2026-01-01T00:03:00Z","repository_commit":commit},
            {"event_id":"n5","tranche_id":"tranche-2","from":"review-pending","to":"approved","event_time":"2026-01-01T00:05:00Z","review_ref":"review-2"},
            {"event_id":"n6","tranche_id":"tranche-2","from":"approved","to":"landed","event_time":"2026-01-01T00:06:00Z"},
        ]
        replay_report = Report()
        states, _ = replay_tranches(
            events,
            fixture["cells"],
            fixture["decisions"],
            fixture["workloads"],
            replay_report,
        )
        if replay_report.integrity:
            return False, replay_report.render()
        subject = tranche_review_subject(
            "tranche-2",
            states["tranche-2"],
            fixture["cells"],
            fixture["results"],
            fixture["deviations"],
            fixture["perf_units"],
            fixture["workloads"],
            fixture["decisions"],
        )
        if subject is None:
            return False, "replacement tranche subject could not be built"
        subject_hash = digest(subject)
        body = canonical_tranche_review_line("tranche-2", "n4", commit, subject_hash)
        fixture["events"] = events
        fixture["states"] = states
        fixture["approval"] = {
            "event_id":"review-2","kind":"tranche-review","subject_id":"tranche-2",
            "subject_digest":subject_hash,"homebrew_commit":HOMEBREW_COMMIT,
            "repository":REPOSITORY,"actor":"reviewer",
            "object_url":f"https://api.github.com/repos/{REPOSITORY}/pulls/8/reviews/10",
            "object_body_digest":text_digest(body),"event_time":"2026-01-01T00:05:00Z",
        }
        fixture["review"] = {
            "id":10,"pull_request_url":f"https://api.github.com/repos/{REPOSITORY}/pulls/8",
            "user":{"login":"reviewer"},"body":body,"state":"APPROVED",
            "commit_id":commit,"submitted_at":"2026-01-01T00:04:00Z",
        }
        fixture["pull"] = {
            "number":8,"user":{"login":"author"},"base":{"ref":"main"},"head":{"sha":commit},
        }
        (ledger / "tranches.jsonl").write_text("".join(json.dumps(row)+"\n" for row in events))
        (ledger / "approvals.jsonl").write_text(json.dumps(fixture["approval"])+"\n")
        result = verify(ledger, offline=False, fetcher=landed_fetcher(ledger, fixture))
        return (
            result.code == 1
            and not result.integrity
            and f"cell {cell} is not in a reviewed landed tranche" not in result.incomplete
        ), result.render()

    outcomes: list[str] = []
    all_ok = True
    with tempfile.TemporaryDirectory(prefix="zapbrew-ledger-self-test-") as temp:
        parent = Path(temp)
        for index, (name, fn) in enumerate(cases):
            ledger = make_temp_ledger(source, parent / f"case-{index}")
            try:
                ok, detail = fn(ledger)
            except Exception as exc:  # Self-test must report a failed injection, not hide it.
                ok, detail = False, f"{type(exc).__name__}: {exc}"
            outcomes.append(f"{'PASS' if ok else 'FAIL'}: {name}" + ("" if ok else f"\n{detail}"))
            all_ok &= ok
    return all_ok, outcomes


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--root",
        type=Path,
        default=Path(__file__).resolve().parent,
        help="ledger directory (default: directory containing verify.py)",
    )
    parser.add_argument("--offline", action="store_true", help="do not query GitHub; online approvals remain incomplete")
    parser.add_argument("--self-test", action="store_true", help="run adversarial temporary-copy injections")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    root = args.root.resolve()
    if args.self_test:
        ok, outcomes = run_self_tests(root)
        print("SELF-TEST PASS" if ok else "SELF-TEST FAIL")
        print("\n".join(outcomes))
        return 0 if ok else 2
    report = verify(root, offline=args.offline)
    print(report.render())
    return report.code


if __name__ == "__main__":
    raise SystemExit(main())
