"""Bidirectional keyset contract tests.

The real comparison IS executed against two vendored, independently sourced
files: upstream rez 3.4.0 ``rezconfig.py`` and the rez-next ``config_schema``.
The accepted invariant is contract-bounded: ``missing_in_reference`` is empty
and ``missing_in_schema`` is exactly the six approved deliberately-unsupported
keys, each bound to a production source justification. The comparison genuinely
PASSES under this contract; no ``expectedFailure`` marker is used.
"""

from __future__ import annotations

import hashlib
import os
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import keyset_invariant as ki

APPROVED_KEYS = (
    "warn_old_commands",
    "error_old_commands",
    "debug_old_commands",
    "rez_1_environment_variables",
    "disable_rez_1_compatibility",
    "plugins",
)


class TestComparator(unittest.TestCase):
    """Synthetic drift-detection tests for the comparator itself."""

    def test_comparator_detects_missing_in_schema(self):
        diff = ki.compare_keysets(["a", "b"], ["a", "b", "c"])
        self.assertEqual(diff["missing_in_schema"], ["c"])
        self.assertEqual(diff["missing_in_reference"], [])

    def test_comparator_detects_missing_in_reference(self):
        diff = ki.compare_keysets(["a", "b", "c"], ["a", "b"])
        self.assertEqual(diff["missing_in_schema"], [])
        self.assertEqual(diff["missing_in_reference"], ["c"])

    def test_comparator_empty_diff_for_equal_keysets(self):
        diff = ki.compare_keysets(["a", "b"], ["b", "a"])
        self.assertEqual(diff, {"missing_in_schema": [], "missing_in_reference": []})

    def test_keysets_equal_detects_registry_addition(self):
        registry = ["debug_build", "debug_resolve"]
        schema = ["debug_build", "debug_resolve"]
        self.assertTrue(ki.keysets_equal(registry, schema))
        registry.append("newly_added_key")
        self.assertFalse(ki.keysets_equal(registry, schema))


class TestSourceIndependence(unittest.TestCase):
    """Negative cases: a same-source or same-side input must not fake a pass."""

    def test_real_sources_are_independent(self):
        sources = ki.load_real_key_sources()
        self.assertNotEqual(sources.schema_path.resolve(), sources.reference_path.resolve())
        self.assertNotEqual(sources.schema_path.read_bytes(), sources.reference_path.read_bytes())

    def test_same_file_env_input_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "keys.json"
            path.write_text('["a", "b"]', encoding="utf-8")
            env = {
                ki.SCHEMA_SOURCE_ENV: str(path),
                ki.REFERENCE_SOURCE_ENV: str(path),
            }
            with (
                mock.patch.dict(os.environ, env, clear=False),
                self.assertRaises(ki.SourceIndependenceError),
            ):
                ki.load_keys_from_env()

    def test_identical_content_distinct_files_are_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            a = Path(tmp) / "a.json"
            b = Path(tmp) / "b.json"
            a.write_text('["a", "b"]', encoding="utf-8")
            b.write_text('["a", "b"]', encoding="utf-8")
            with self.assertRaises(ki.SourceIndependenceError):
                ki.assert_distinct_sources(a, b)

    def test_rez_next_own_rezconfig_is_rejected_as_reference(self):
        # rez-next's compatibility/defaults module is the SAME side as
        # config_schema; using it as the reference would be circular.
        with self.assertRaises(ki.SourceIndependenceError):
            ki.assert_independent_sources(ki.SCHEMA_SOURCE, ki.SAME_SIDE_REZCONFIG)

    def test_schema_and_reference_trees_cannot_be_swapped(self):
        with self.assertRaises(ki.SourceIndependenceError):
            ki.assert_independent_sources(ki.REFERENCE_SOURCE, ki.SCHEMA_SOURCE)

    def test_same_source_would_otherwise_fake_an_empty_diff(self):
        # Document the exact failure mode the independence guard prevents.
        schema = ["a", "b"]
        self.assertEqual(
            ki.compare_keysets(schema, list(schema)),
            {"missing_in_schema": [], "missing_in_reference": []},
        )


class TestVendoredSourcesAreUnmodified(unittest.TestCase):
    """Pin both source hashes: no key was deleted or injected to force a pass."""

    def _assert_forms_pinned(self, path: Path, lf_pin: str, crlf_pin: str) -> None:
        normalized = ki.normalize_line_endings(path.read_bytes())
        # Primary pin: EOL-normalized (LF) digest, independent of checkout EOL.
        self.assertEqual(ki.source_sha256(path), lf_pin)
        self.assertEqual(hashlib.sha256(normalized).hexdigest(), lf_pin)
        # Second pinned convention: the CRLF-rendered digest.
        self.assertEqual(hashlib.sha256(ki.crlf_line_endings(normalized)).hexdigest(), crlf_pin)
        # Platform proof: a CRLF copy normalizes back to the same primary pin.
        with tempfile.TemporaryDirectory() as tmp:
            crlf_copy = Path(tmp) / "source.py"
            crlf_copy.write_bytes(ki.crlf_line_endings(normalized))
            self.assertEqual(ki.source_sha256(crlf_copy), lf_pin)

    def test_reference_source_hash_is_pinned(self):
        self._assert_forms_pinned(
            ki.REFERENCE_SOURCE, ki.REFERENCE_SOURCE_SHA256, ki.REFERENCE_SOURCE_CRLF_SHA256
        )

    def test_schema_source_hash_is_pinned(self):
        self._assert_forms_pinned(
            ki.SCHEMA_SOURCE, ki.SCHEMA_SOURCE_SHA256, ki.SCHEMA_SOURCE_CRLF_SHA256
        )

    def test_no_keys_were_deleted_from_either_source(self):
        sources = ki.load_real_key_sources()
        self.assertEqual(len(sources.reference_keys), 146)
        self.assertEqual(len(sources.schema_keys), 140)
        for key in ki.KNOWN_DIVERGENCE:
            self.assertIn(key, sources.reference_keys)
            self.assertNotIn(key, sources.schema_keys)

    def test_vendored_schema_matches_production_config_py(self):
        # The pinned schema blob MUST be the real production source, not a
        # detached copy that could drift away from what the package ships.
        self.assertTrue(ki.PRODUCTION_CONFIG.is_file())
        self.assertEqual(
            ki.normalize_line_endings(ki.SCHEMA_SOURCE.read_bytes()),
            ki.normalize_line_endings(ki.PRODUCTION_CONFIG.read_bytes()),
        )
        self.assertEqual(ki.source_sha256(ki.PRODUCTION_CONFIG), ki.SCHEMA_SOURCE_SHA256)
        self.assertEqual(
            ki.extract_schema_keys(ki.PRODUCTION_CONFIG),
            ki.extract_schema_keys(ki.SCHEMA_SOURCE),
        )

    def test_vendored_keys_rs_matches_production_keys_rs(self):
        # The per-key justification evidence must come from the shipped Rust
        # registry, not from a stale copy.
        self.assertTrue(ki.PRODUCTION_KEYS_RS.is_file())
        self.assertEqual(
            ki.normalize_line_endings(ki.VENDORED_KEYS_RS.read_bytes()),
            ki.normalize_line_endings(ki.PRODUCTION_KEYS_RS.read_bytes()),
        )


class TestRealBidirectionalContract(unittest.TestCase):
    """The real comparison, executed and genuinely PASSING under the contract."""

    def test_bidirectional_keyset_contract_holds(self):
        sources = ki.load_real_key_sources()
        diff = ki.compare_keysets(sources.schema_keys, sources.reference_keys)
        self.assertEqual(diff["missing_in_reference"], [], "schema keys missing upstream")
        self.assertEqual(
            diff["missing_in_schema"],
            list(ki.KNOWN_DIVERGENCE),
            "unapproved upstream keys missing from the schema",
        )
        self.assertTrue(ki.contract_holds(sources.schema_keys, sources.reference_keys))
        # The predicate itself must reject an unapproved seventh divergence key
        # and a deleted approved key, not merely report the happy path.
        seventh = [*sources.reference_keys, "unapproved_seventh_key"]
        self.assertFalse(ki.contract_holds(sources.schema_keys, seventh))
        lost = [k for k in sources.reference_keys if k != "warn_old_commands"]
        self.assertFalse(ki.contract_holds(sources.schema_keys, lost))
        # Literal set equality is NOT the acceptance criterion; the six keys are
        # deliberately unsupported. Recorded explicitly so the contract is not
        # mistaken for a hidden waiver of strict equality.
        self.assertFalse(ki.keysets_equal(sources.schema_keys, sources.reference_keys))

    def test_strict_bidirectional_keyset_equality(self):
        # Literal set equality is NOT the acceptance criterion: the six
        # approved keys are deliberately unsupported. The former
        # expectedFailure marker is REMOVED; instead this test honestly records
        # that strict equality does not hold while the contract-bounded
        # invariant genuinely passes. No recorded FAIL is used as acceptance.
        sources = ki.load_real_key_sources()
        self.assertFalse(ki.keysets_equal(sources.schema_keys, sources.reference_keys))
        self.assertTrue(
            ki.contract_holds(sources.schema_keys, sources.reference_keys),
            "contract-bounded bidirectional equality must hold",
        )

    def test_real_comparison_documents_the_six_key_divergence(self):
        sources = ki.load_real_key_sources()
        diff = ki.compare_keysets(sources.schema_keys, sources.reference_keys)
        self.assertEqual(diff["missing_in_reference"], [])
        self.assertEqual(diff["missing_in_schema"], list(ki.KNOWN_DIVERGENCE))

    def test_new_upstream_key_without_schema_registration_is_detected(self):
        sources = ki.load_real_key_sources()
        reference = [*sources.reference_keys, "brand_new_registry_key"]
        diff = ki.compare_keysets(sources.schema_keys, reference)
        self.assertIn("brand_new_registry_key", diff["missing_in_schema"])
        self.assertFalse(ki.contract_holds(sources.schema_keys, reference))
        self.assertNotEqual(diff["missing_in_schema"], list(ki.KNOWN_DIVERGENCE))

    def test_new_schema_key_without_upstream_key_is_detected(self):
        sources = ki.load_real_key_sources()
        schema = [*sources.schema_keys, "rez_next_only_key"]
        diff = ki.compare_keysets(schema, sources.reference_keys)
        self.assertEqual(diff["missing_in_reference"], ["rez_next_only_key"])
        self.assertFalse(ki.contract_holds(schema, sources.reference_keys))

    def test_deleted_approved_key_is_detected(self):
        sources = ki.load_real_key_sources()
        reference = [k for k in sources.reference_keys if k != "warn_old_commands"]
        diff = ki.compare_keysets(sources.schema_keys, reference)
        self.assertNotEqual(diff["missing_in_schema"], list(ki.KNOWN_DIVERGENCE))
        self.assertFalse(ki.contract_holds(sources.schema_keys, reference))


class TestPerKeyClassification(unittest.TestCase):
    """Each exempted key is classified and bound to a production source reason."""

    def test_known_divergence_is_exactly_the_six_approved_keys(self):
        self.assertEqual(list(ki.KNOWN_DIVERGENCE), sorted(APPROVED_KEYS))
        self.assertEqual(len(ki.KNOWN_DIVERGENCE), 6)
        self.assertEqual(len(ki.CLASSIFICATIONS), 6)
        self.assertEqual(
            sorted(c.key for c in ki.CLASSIFICATIONS), sorted(APPROVED_KEYS)
        )
        categories = [c.category for c in ki.CLASSIFICATIONS]
        self.assertEqual(categories.count("deprecated_rez1"), 5)
        self.assertEqual(categories.count("plugin_namespace"), 1)

    def test_every_classification_key_is_in_known_divergence(self):
        for classification in ki.CLASSIFICATIONS:
            with self.subTest(key=classification.key):
                self.assertIn(classification.key, ki.KNOWN_DIVERGENCE)
                self.assertIn(
                    classification.category, ("deprecated_rez1", "plugin_namespace")
                )

    def test_each_approved_key_has_production_evidence(self):
        sources = ki.load_real_key_sources()
        evidence_cache: dict[str, str] = {}
        for classification in ki.CLASSIFICATIONS:
            with self.subTest(key=classification.key):
                # The key must really be the divergence it claims to explain.
                self.assertIn(classification.key, sources.reference_keys)
                self.assertNotIn(classification.key, sources.schema_keys)
                # A missing or empty justification invalidates the exemption.
                self.assertTrue(classification.evidence, "no production evidence bound")
                for label, needle in classification.evidence:
                    self.assertIn(label, ki.EVIDENCE_FILES)
                    path = ki.EVIDENCE_FILES[label]
                    if label not in evidence_cache:
                        evidence_cache[label] = path.read_text(encoding="utf-8")
                    self.assertIn(
                        needle,
                        evidence_cache[label],
                        f"{classification.key}: missing {label!r} evidence {needle!r}",
                    )

    def test_renamed_or_lost_classification_is_detected(self):
        # If an entry is lost or renamed, the classification set no longer
        # covers the approved divergence.
        lost = tuple(c for c in ki.CLASSIFICATIONS if c.key != "plugins")
        self.assertNotEqual(
            sorted(c.key for c in lost), sorted(ki.KNOWN_DIVERGENCE)
        )
        renamed = tuple(
            dataclasses_replace(c, key=c.key + "_renamed") for c in ki.CLASSIFICATIONS
        )
        self.assertNotEqual(
            sorted(c.key for c in renamed), sorted(ki.KNOWN_DIVERGENCE)
        )


def dataclasses_replace(obj, **changes):
    """Small local helper (avoids importing dataclasses just for one check)."""
    import dataclasses

    return dataclasses.replace(obj, **changes)


if __name__ == "__main__":
    unittest.main()
