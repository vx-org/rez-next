"""Execute named tests against mutated private copies of the keyset harness.

Each mutation is applied to a PRIVATE temporary copy of the harness (including
the vendored, independent sources); the named unittest is then executed in a
fresh interpreter (``python -m unittest <dotted.name>``). A mutation counts as
killed only when that exact test runs and fails, while the unmutated copy of
the same test passes. Nothing in the repository is modified.

The negative cases cover: a deleted/renamed/injected approved key, a seventh
divergence key, a schema-only key, same-source and swapped-tree inputs, each of
the four pinned source blobs, and missing per-key production evidence.
"""

from __future__ import annotations

import dataclasses
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

HARNESS_DIR = Path(__file__).resolve().parent
REPO_ROOT = HARNESS_DIR.parents[1]
HARNESS_FILES = (
    "keyset_invariant.py",
    "test_keyset_invariant.py",
    "run_real_keyset_comparison.py",
)

BASELINE_TESTS = (
    "test_keyset_invariant.TestComparator.test_comparator_detects_missing_in_schema",
    "test_keyset_invariant.TestComparator.test_comparator_detects_missing_in_reference",
    "test_keyset_invariant.TestComparator.test_comparator_empty_diff_for_equal_keysets",
    "test_keyset_invariant.TestComparator.test_keysets_equal_detects_registry_addition",
    "test_keyset_invariant.TestSourceIndependence.test_real_sources_are_independent",
    "test_keyset_invariant.TestSourceIndependence.test_same_file_env_input_is_rejected",
    "test_keyset_invariant.TestSourceIndependence.test_identical_content_distinct_files_are_rejected",
    "test_keyset_invariant.TestSourceIndependence.test_rez_next_own_rezconfig_is_rejected_as_reference",
    "test_keyset_invariant.TestSourceIndependence.test_schema_and_reference_trees_cannot_be_swapped",
    "test_keyset_invariant.TestSourceIndependence.test_same_source_would_otherwise_fake_an_empty_diff",
    "test_keyset_invariant.TestVendoredSourcesAreUnmodified.test_reference_source_hash_is_pinned",
    "test_keyset_invariant.TestVendoredSourcesAreUnmodified.test_schema_source_hash_is_pinned",
    "test_keyset_invariant.TestVendoredSourcesAreUnmodified.test_no_keys_were_deleted_from_either_source",
    "test_keyset_invariant.TestVendoredSourcesAreUnmodified.test_vendored_schema_matches_production_config_py",
    "test_keyset_invariant.TestVendoredSourcesAreUnmodified.test_vendored_keys_rs_matches_production_keys_rs",
    "test_keyset_invariant.TestRealBidirectionalContract.test_bidirectional_keyset_contract_holds",
    "test_keyset_invariant.TestRealBidirectionalContract.test_strict_bidirectional_keyset_equality",
    "test_keyset_invariant.TestRealBidirectionalContract.test_real_comparison_documents_the_six_key_divergence",
    "test_keyset_invariant.TestRealBidirectionalContract.test_new_upstream_key_without_schema_registration_is_detected",
    "test_keyset_invariant.TestRealBidirectionalContract.test_new_schema_key_without_upstream_key_is_detected",
    "test_keyset_invariant.TestRealBidirectionalContract.test_deleted_approved_key_is_detected",
    "test_keyset_invariant.TestPerKeyClassification.test_known_divergence_is_exactly_the_six_approved_keys",
    "test_keyset_invariant.TestPerKeyClassification.test_every_classification_key_is_in_known_divergence",
    "test_keyset_invariant.TestPerKeyClassification.test_each_approved_key_has_production_evidence",
    "test_keyset_invariant.TestPerKeyClassification.test_renamed_or_lost_classification_is_detected",
)

REF = "upstream/rez-3.4.0/src/rez/rezconfig.py"
CFG = "upstream/rez-next@2405006/crates/rez-next-python/python/rez_next/config.py"


@dataclasses.dataclass(frozen=True)
class Mutation:
    label: str
    target: str
    old: str
    new: str
    test: str


MUTATIONS: tuple[Mutation, ...] = (
    Mutation(
        "B missing_in_schema dropped",
        "keyset_invariant.py",
        '"missing_in_schema": sorted(reference - schema),',
        '"missing_in_schema": [],',
        "test_keyset_invariant.TestComparator.test_comparator_detects_missing_in_schema",
    ),
    Mutation(
        "B missing_in_reference dropped",
        "keyset_invariant.py",
        '"missing_in_reference": sorted(schema - reference),',
        '"missing_in_reference": [],',
        "test_keyset_invariant.TestComparator.test_comparator_detects_missing_in_reference",
    ),
    Mutation(
        "B equality short-circuited to True",
        "keyset_invariant.py",
        '    return not diff["missing_in_schema"] and not diff["missing_in_reference"]',
        "    return True",
        "test_keyset_invariant.TestComparator.test_keysets_equal_detects_registry_addition",
    ),
    Mutation(
        "B contract ignores missing_in_schema",
        "keyset_invariant.py",
        '        and diff["missing_in_schema"] == list(KNOWN_DIVERGENCE)',
        "        and True",
        "test_keyset_invariant.TestRealBidirectionalContract.test_bidirectional_keyset_contract_holds",
    ),
    Mutation(
        "B env-source independence check removed",
        "keyset_invariant.py",
        "    assert_distinct_sources(Path(schema_path), Path(reference_path))\n"
        "    schema = load_keys(SCHEMA_SOURCE_ENV)",
        "    pass  # independence check removed\n    schema = load_keys(SCHEMA_SOURCE_ENV)",
        "test_keyset_invariant.TestSourceIndependence.test_same_file_env_input_is_rejected",
    ),
    Mutation(
        "B same-side reference tree check removed",
        "keyset_invariant.py",
        "    if not reference.is_relative_to(REFERENCE_TREE.resolve()):",
        "    if False and not reference.is_relative_to(REFERENCE_TREE.resolve()):",
        "test_keyset_invariant.TestSourceIndependence.test_rez_next_own_rezconfig_is_rejected_as_reference",
    ),
    Mutation(
        "B swapped-tree membership checks removed",
        "keyset_invariant.py",
        "    if not schema.is_relative_to(SCHEMA_TREE.resolve()):\n"
        '        raise SourceIndependenceError("schema source is not from the rez-next tree")\n'
        "    if not reference.is_relative_to(REFERENCE_TREE.resolve()):",
        "    if False:\n"
        '        raise SourceIndependenceError("schema source is not from the rez-next tree")\n'
        "    if False and not reference.is_relative_to(REFERENCE_TREE.resolve()):",
        "test_keyset_invariant.TestSourceIndependence.test_schema_and_reference_trees_cannot_be_swapped",
    ),
    Mutation(
        "B same source used for both sides",
        "keyset_invariant.py",
        "    reference_path: Path = REFERENCE_SOURCE,",
        "    reference_path: Path = SCHEMA_SOURCE,",
        "test_keyset_invariant.TestSourceIndependence.test_real_sources_are_independent",
    ),
    Mutation(
        "B one approved key deleted from allowlist",
        "keyset_invariant.py",
        '    "warn_old_commands",\n    "error_old_commands",',
        '    "error_old_commands",',
        "test_keyset_invariant.TestPerKeyClassification.test_known_divergence_is_exactly_the_six_approved_keys",
    ),
    Mutation(
        "B seventh unapproved divergence key injected",
        "keyset_invariant.py",
        '    "disable_rez_1_compatibility",\n)',
        '    "disable_rez_1_compatibility",\n    "seventh_unapproved_key",\n)',
        "test_keyset_invariant.TestRealBidirectionalContract.test_bidirectional_keyset_contract_holds",
    ),
    Mutation(
        "B reference LF source hash pin tampered",
        "keyset_invariant.py",
        '"0bf79731e179ca0815b5364ffd810e23dbe7c974f2fd2aca78a0742b15778599"',
        '"0000000000000000000000000000000000000000000000000000000000000000"',
        "test_keyset_invariant.TestVendoredSourcesAreUnmodified.test_reference_source_hash_is_pinned",
    ),
    Mutation(
        "B schema LF source hash pin tampered",
        "keyset_invariant.py",
        '"eb812d59fed0f05d78862d6bc3f93cf58b1cd306708665f9304940fcbc23fb75"',
        '"0000000000000000000000000000000000000000000000000000000000000000"',
        "test_keyset_invariant.TestVendoredSourcesAreUnmodified.test_schema_source_hash_is_pinned",
    ),
    Mutation(
        "B reference CRLF source hash pin tampered",
        "keyset_invariant.py",
        '"c634be4d326be7cf41d1545e3b00b0495074281f0e9e09e384ac69b0ba852e05"',
        '"0000000000000000000000000000000000000000000000000000000000000000"',
        "test_keyset_invariant.TestVendoredSourcesAreUnmodified.test_reference_source_hash_is_pinned",
    ),
    Mutation(
        "B schema CRLF source hash pin tampered",
        "keyset_invariant.py",
        '"be12a3bd138cd021278d2ff48b45ea445a8fbc9da957da5a1abfc78f7c170c7b"',
        '"0000000000000000000000000000000000000000000000000000000000000000"',
        "test_keyset_invariant.TestVendoredSourcesAreUnmodified.test_schema_source_hash_is_pinned",
    ),
    Mutation(
        "B one approved key deleted from upstream reference",
        REF,
        "warn_old_commands = True",
        "# warn_old_commands removed (mutation)",
        "test_keyset_invariant.TestRealBidirectionalContract.test_real_comparison_documents_the_six_key_divergence",
    ),
    Mutation(
        "B seventh upstream key injected into reference",
        REF,
        "warn_old_commands = True",
        "warn_old_commands = True\nbrand_new_upstream_key = 1",
        "test_keyset_invariant.TestRealBidirectionalContract.test_bidirectional_keyset_contract_holds",
    ),
    Mutation(
        "B schema-only key injected into config_schema",
        CFG,
        "config_schema = {\n",
        'config_schema = {\n    "brand_new_schema_key": Str,\n',
        "test_keyset_invariant.TestRealBidirectionalContract.test_bidirectional_keyset_contract_holds",
    ),
    Mutation(
        "B per-key production evidence removed",
        CFG,
        '    "warn_old_commands": _Deprecation("the future"),\n',
        "",
        "test_keyset_invariant.TestPerKeyClassification.test_each_approved_key_has_production_evidence",
    ),
    Mutation(
        "B per-key evidence needle points at a missing reason",
        "keyset_invariant.py",
        '            ("reference", "warn_old_commands = True"),',
        '            ("reference", "THIS_REASON_IS_MISSING"),',
        "test_keyset_invariant.TestPerKeyClassification.test_each_approved_key_has_production_evidence",
    ),
)


def copy_harness(dest: Path) -> None:
    """Copy the harness (including vendored independent sources) into scratch."""
    for name in HARNESS_FILES:
        shutil.copy2(HARNESS_DIR / name, dest / name)
    shutil.copytree(HARNESS_DIR / "upstream", dest / "upstream")


def run_named_test(workdir: Path, test: str) -> subprocess.CompletedProcess[str]:
    """Execute one exact dotted test name in a fresh interpreter."""
    env = {
        **os.environ,
        "PYTHONPATH": str(workdir),
        "KEYSET_CONTRACT_REPO_ROOT": str(REPO_ROOT),
    }
    return subprocess.run(
        [sys.executable, "-m", "unittest", "-v", test],
        cwd=workdir,
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )


def combined_output(result: subprocess.CompletedProcess[str]) -> str:
    return result.stdout + result.stderr


def baseline_passes(workdir: Path, test: str) -> bool:
    result = run_named_test(workdir, test)
    output = combined_output(result)
    return result.returncode == 0 and "Ran 1 test" in output and "OK" in output


def run_real_comparison(workdir: Path) -> subprocess.CompletedProcess[str]:
    """Run the standalone real comparison; it must exit 0 under the contract."""
    return subprocess.run(
        [sys.executable, "-I", str(workdir / "run_real_keyset_comparison.py")],
        cwd=workdir,
        capture_output=True,
        text=True,
        check=False,
        env={**os.environ, "KEYSET_CONTRACT_REPO_ROOT": str(REPO_ROOT)},
    )


def mutation_killed(workdir: Path, test: str) -> bool:
    result = run_named_test(workdir, test)
    output = combined_output(result)
    ran = "Ran 1 test" in output
    failed = "FAILED" in output or "ERROR" in output
    return result.returncode != 0 and ran and failed


def main() -> int:
    print(f"harness: {HARNESS_DIR}")
    print(f"python:  {sys.executable}")
    ok = True

    print("\n[1/3] baseline: every named test must pass unmutated")
    with tempfile.TemporaryDirectory() as base_tmp:
        base_dir = Path(base_tmp)
        copy_harness(base_dir)
        for test in BASELINE_TESTS:
            passed = baseline_passes(base_dir, test)
            print(f"  {'GREEN' if passed else 'FAIL  '}  {test}")
            ok = ok and passed

    print("\n[2/3] real bidirectional comparison: must exit 0 under the contract")
    with tempfile.TemporaryDirectory() as real_tmp:
        real_dir = Path(real_tmp)
        copy_harness(real_dir)
        result = run_real_comparison(real_dir)
        output = combined_output(result)
        contract_pass = (
            result.returncode == 0
            and "RESULT: PASS" in output
            and all(
                key in output
                for key in (
                    "debug_old_commands",
                    "disable_rez_1_compatibility",
                    "error_old_commands",
                    "plugins",
                    "rez_1_environment_variables",
                    "warn_old_commands",
                )
            )
        )
        print(
            f"  {'PASS (contract)' if contract_pass else 'UNEXPECTED'}  "
            "run_real_keyset_comparison.py"
        )
        ok = ok and contract_pass

    print("\n[3/3] mutation: each named test must go red under its mutation")
    for mutation in MUTATIONS:
        with tempfile.TemporaryDirectory() as mut_tmp:
            mut_dir = Path(mut_tmp)
            copy_harness(mut_dir)
            path = mut_dir / mutation.target
            text = path.read_text(encoding="utf-8")
            count = text.count(mutation.old)
            if count != 1:
                print(f"  ANCHOR-ERROR ({count})  {mutation.label}")
                ok = False
                continue
            path.write_text(text.replace(mutation.old, mutation.new, 1), encoding="utf-8")
            killed = mutation_killed(mut_dir, mutation.test)
            print(f"  {'KILLED  ' if killed else 'SURVIVED'}  {mutation.label}")
            ok = ok and killed

    print("\nRESULT:", "ALL MUTATIONS KILLED" if ok else "INCOMPLETE / FAILED")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
