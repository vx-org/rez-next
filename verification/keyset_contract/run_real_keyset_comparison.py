"""Execute the real bidirectional keyset comparison and enforce the contract.

Prints both directional diffs, verifies the pinned, independent sources and
the per-key production evidence, and exits **0** when the contract holds:

* ``missing_in_reference`` is empty;
* ``missing_in_schema`` is exactly the six approved deliberately-unsupported
  keys (five deprecated rez-1 settings + the ``plugins`` namespace).

Any new/removed/renamed key, any non-independent source, a tampered pinned
blob or a missing per-key justification makes this command exit **1**. It never
deletes a key, never reuses one source for both sides and never reports a
recorded FAIL as acceptance.
"""

from __future__ import annotations

import sys
from pathlib import Path

# Keep the harness importable under `python -I` (isolated mode does not prepend
# the script directory to sys.path).
sys.path.insert(0, str(Path(__file__).resolve().parent))

import keyset_invariant as ki  # noqa: E402


def _verify_pins() -> list[str]:
    """Return a list of pin violations (empty when all four pins hold)."""
    problems: list[str] = []
    checks = (
        (ki.REFERENCE_SOURCE, ki.REFERENCE_SOURCE_SHA256, "reference LF pin"),
        (ki.SCHEMA_SOURCE, ki.SCHEMA_SOURCE_SHA256, "schema LF pin"),
    )
    for path, pin, label in checks:
        if ki.source_sha256(path) != pin:
            problems.append(f"{label}: pinned source blob drifted ({path})")
    if ki.source_sha256(ki.PRODUCTION_CONFIG) != ki.SCHEMA_SOURCE_SHA256:
        problems.append("pinned schema blob differs from production config.py")
    if ki.source_sha256(ki.PRODUCTION_KEYS_RS) != ki.source_sha256(ki.VENDORED_KEYS_RS):
        problems.append("vendored keys.rs differs from production keys.rs")
    return problems


def _verify_evidence() -> list[str]:
    """Return a list of missing per-key production justifications."""
    problems: list[str] = []
    cache: dict[str, str] = {}
    for classification in ki.CLASSIFICATIONS:
        if not classification.evidence:
            problems.append(f"{classification.key}: no production evidence bound")
        for label, needle in classification.evidence:
            path = ki.EVIDENCE_FILES[label]
            if label not in cache:
                cache[label] = path.read_text(encoding="utf-8")
            if needle not in cache[label]:
                problems.append(f"{classification.key}: missing {label} evidence {needle!r}")
    return problems


def main() -> int:
    problems: list[str] = []
    try:
        sources = ki.load_real_key_sources()
    except ki.SourceIndependenceError as exc:
        print(f"RESULT: FAIL (source independence: {exc})")
        return 1

    diff = ki.compare_keysets(sources.schema_keys, sources.reference_keys)

    print(f"schema source    : {sources.schema_path}")
    print(f"reference source : {sources.reference_path}")
    print(f"schema keys      : {len(sources.schema_keys)}")
    print(f"reference keys   : {len(sources.reference_keys)}")
    print(f"missing_in_schema ({len(diff['missing_in_schema'])}):")
    for key in diff["missing_in_schema"]:
        print(f"  - {key}")
    print(f"missing_in_reference ({len(diff['missing_in_reference'])}):")
    for key in diff["missing_in_reference"]:
        print(f"  - {key}")

    if diff["missing_in_reference"]:
        problems.append("schema has keys absent from the upstream reference")
    if diff["missing_in_schema"] != list(ki.KNOWN_DIVERGENCE):
        problems.append("missing_in_schema is not exactly the six approved keys")
    problems.extend(_verify_pins())
    problems.extend(_verify_evidence())

    if problems:
        print("RESULT: FAIL (contract violated)")
        for problem in problems:
            print(f"  - {problem}")
        return 1

    print("RESULT: PASS (contract-bounded bidirectional equality holds)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
