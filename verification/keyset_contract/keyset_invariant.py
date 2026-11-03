"""Bidirectional keyset contract for the rez-next configuration schema.

Two INDEPENDENT, hash-pinned authoritative sources are vendored read-only:

* upstream rez **3.4.0** ``src/rez/rezconfig.py`` (146 top-level settings);
* the rez-next ``config_schema`` dict (140 keys) from ``config.py``, pinned at
  the schema delivery and verified byte-for-byte against the production file
  shipped in this repository.

The accepted invariant is *contract-bounded* bidirectional equality, not
literal set equality:

* ``missing_in_reference`` (schema keys absent upstream) is **empty**;
* ``missing_in_schema`` (upstream keys absent from the schema) is **exactly**
  the six approved deliberately-unsupported keys: five explicitly deprecated
  rez-1 compatibility settings and the ``plugins`` extension namespace.

Each of the six keys is bound to its production source justification (upstream
deprecation markers, ``_deprecated_settings``, ``keys.rs`` deprecation flags and
the dotted plugin-namespace documentation). Any new, removed or renamed key,
any source non-independence, and any drift in a pinned source blob is a
failure. No key is deleted from either source, no same-side source is reused,
and no no-op validator is added to manufacture equality.
"""

from __future__ import annotations

import ast
import dataclasses
import hashlib
import json
import os
from pathlib import Path

ROOT = Path(__file__).resolve().parent
# Overridable so mutation runs in a private copy still read the real production
# files. Defaults to the repository root (two levels above this directory).
REPO_ROOT = Path(
    os.environ.get("KEYSET_CONTRACT_REPO_ROOT", str(ROOT.parents[1]))
)

REFERENCE_TREE = ROOT / "upstream" / "rez-3.4.0"
SCHEMA_TREE = ROOT / "upstream" / "rez-next@2405006"

REFERENCE_SOURCE = REFERENCE_TREE / "src" / "rez" / "rezconfig.py"
SCHEMA_SOURCE = (
    SCHEMA_TREE
    / "crates"
    / "rez-next-python"
    / "python"
    / "rez_next"
    / "config.py"
)
# rez-next's own compatibility/defaults module: the SAME side as config_schema,
# so it is explicitly rejected as an independent reference.
SAME_SIDE_REZCONFIG = (
    SCHEMA_TREE
    / "crates"
    / "rez-next-python"
    / "python"
    / "rez_next"
    / "rezconfig.py"
)
# Production evidence files (vendored copies, proven identical to the real ones).
VENDORED_KEYS_RS = SCHEMA_TREE / "crates" / "rez-next-config" / "src" / "keys.rs"

# The live production counterparts in this repository.
PRODUCTION_CONFIG = (
    REPO_ROOT / "crates" / "rez-next-python" / "python" / "rez_next" / "config.py"
)
PRODUCTION_KEYS_RS = REPO_ROOT / "crates" / "rez-next-config" / "src" / "keys.rs"

# Retained for synthetic comparator tests only. The real comparison reads the
# vendored sources directly and never uses these environment variables.
SCHEMA_SOURCE_ENV = "REZNEXT_SCHEMA_KEYS"
REFERENCE_SOURCE_ENV = "REZ_340_REZCONFIG_KEYS"

# Five explicitly deprecated rez-1 compatibility settings.
DEPRECATED_REZ1_KEYS: tuple[str, ...] = (
    "warn_old_commands",
    "error_old_commands",
    "debug_old_commands",
    "rez_1_environment_variables",
    "disable_rez_1_compatibility",
)
# One plugin extension namespace (dotted-path container, not a flat scalar).
PLUGIN_NAMESPACE_KEYS: tuple[str, ...] = ("plugins",)

# The exact approved divergence, pinned so a new upstream key added without
# schema registration, a deleted key, or a renamed key turns the contract red.
KNOWN_DIVERGENCE: tuple[str, ...] = tuple(
    sorted(DEPRECATED_REZ1_KEYS + PLUGIN_NAMESPACE_KEYS)
)

# SHA-256 of the two vendored authoritative sources. The repository blobs are
# LF; the LF-normalized digest is the primary pin and the CRLF-rendered digest
# is a second pin, so the check is independent of the checkout's line endings.
# Pinning these proves no key was removed from, or injected into, either source.
REFERENCE_SOURCE_SHA256 = "0bf79731e179ca0815b5364ffd810e23dbe7c974f2fd2aca78a0742b15778599"
SCHEMA_SOURCE_SHA256 = "eb812d59fed0f05d78862d6bc3f93cf58b1cd306708665f9304940fcbc23fb75"
REFERENCE_SOURCE_CRLF_SHA256 = "c634be4d326be7cf41d1545e3b00b0495074281f0e9e09e384ac69b0ba852e05"
SCHEMA_SOURCE_CRLF_SHA256 = "be12a3bd138cd021278d2ff48b45ea445a8fbc9da957da5a1abfc78f7c170c7b"


@dataclasses.dataclass(frozen=True)
class KeyClassification:
    """One approved divergent key, its category and its production evidence."""

    key: str
    category: str  # "deprecated_rez1" | "plugin_namespace"
    # (evidence label, required source needle). Every needle must be present in
    # the mapped production file; a missing needle invalidates the exemption.
    evidence: tuple[tuple[str, str], ...]


# Evidence labels map to the three production source files used as justification.
EVIDENCE_FILES = {
    "reference": REFERENCE_SOURCE,
    "config_py": SCHEMA_SOURCE,
    "keys_rs": VENDORED_KEYS_RS,
}

CLASSIFICATIONS: tuple[KeyClassification, ...] = (
    KeyClassification(
        key="warn_old_commands",
        category="deprecated_rez1",
        evidence=(
            ("reference", "warn_old_commands = True"),
            ("reference", ".. deprecated:: 2.114.0"),
            ("config_py", '"warn_old_commands": _Deprecation('),
        ),
    ),
    KeyClassification(
        key="error_old_commands",
        category="deprecated_rez1",
        evidence=(
            ("reference", "error_old_commands = False"),
            ("reference", ".. deprecated:: 2.114.0"),
            ("config_py", '"error_old_commands": _Deprecation('),
        ),
    ),
    KeyClassification(
        key="debug_old_commands",
        category="deprecated_rez1",
        evidence=(
            ("reference", "debug_old_commands = False"),
            ("reference", "This currently has no effect"),
            ("reference", ".. deprecated:: 2.114.0"),
        ),
    ),
    KeyClassification(
        key="rez_1_environment_variables",
        category="deprecated_rez1",
        evidence=(
            ("reference", "rez_1_environment_variables = False"),
            ("reference", ".. deprecated:: 2.114.0"),
            ("config_py", '"rez_1_environment_variables": _Deprecation('),
        ),
    ),
    KeyClassification(
        key="disable_rez_1_compatibility",
        category="deprecated_rez1",
        evidence=(
            ("reference", "disable_rez_1_compatibility = True"),
            ("reference", ".. deprecated:: 2.114.0"),
            ("config_py", '"disable_rez_1_compatibility": _Deprecation('),
            ("keys_rs", 'name: "disable_rez_1_compatibility"'),
            ("keys_rs", "deprecated: true"),
        ),
    ),
    KeyClassification(
        key="plugins",
        category="plugin_namespace",
        evidence=(
            ("reference", "plugins = {}"),
            ("reference", "common to all plugins of that type"),
            ("config_py", "class _PluginConfigs:"),
            ("config_py", "plugins: dict = {}"),
            ("keys_rs", "Plugin settings are addressed as `plugins.<plugin>.<setting>`"),
            ("keys_rs", "registered but not schema-validated"),
        ),
    ),
)


def normalize_line_endings(data: bytes) -> bytes:
    """Collapse CRLF/CR to LF so a content hash is EOL-independent."""
    return data.replace(b"\r\n", b"\n").replace(b"\r", b"\n")


def crlf_line_endings(data: bytes) -> bytes:
    """Render LF-normalized bytes as CRLF (the second pinned convention)."""
    return normalize_line_endings(data).replace(b"\n", b"\r\n")


def source_sha256(path: Path) -> str:
    """EOL-normalized SHA-256 of a vendored source file."""
    return hashlib.sha256(normalize_line_endings(Path(path).read_bytes())).hexdigest()


class SourceIndependenceError(ValueError):
    """Raised when the two key lists are not independently sourced."""


@dataclasses.dataclass(frozen=True)
class RealKeySources:
    """The two real key lists together with the files they were read from."""

    schema_keys: list[str]
    reference_keys: list[str]
    schema_path: Path
    reference_path: Path


def load_keys(env_var: str) -> list[str] | None:
    """Load an authoritative key list from a JSON file named by ``env_var``.

    Synthetic comparator tests use this. It never feeds the real comparison.
    """
    path = os.environ.get(env_var)
    if not path or not Path(path).is_file():
        return None
    payload = json.loads(Path(path).read_text(encoding="utf-8"))
    if not isinstance(payload, list) or not all(isinstance(item, str) for item in payload):
        raise ValueError(f"{env_var} must point to a JSON array of strings")
    return payload


def compare_keysets(schema_keys: list[str], reference_keys: list[str]) -> dict[str, list[str]]:
    """Return both directional diffs, sorted for a stable contract comparison."""
    schema = set(schema_keys)
    reference = set(reference_keys)
    return {
        "missing_in_schema": sorted(reference - schema),
        "missing_in_reference": sorted(schema - reference),
    }


def keysets_equal(schema_keys: list[str], reference_keys: list[str]) -> bool:
    """True only when neither directional diff has any key (literal equality)."""
    diff = compare_keysets(schema_keys, reference_keys)
    return not diff["missing_in_schema"] and not diff["missing_in_reference"]


def contract_holds(schema_keys: list[str], reference_keys: list[str]) -> bool:
    """The accepted invariant: no unauthorized drift in either direction."""
    diff = compare_keysets(schema_keys, reference_keys)
    return (
        diff["missing_in_reference"] == []
        and diff["missing_in_schema"] == list(KNOWN_DIVERGENCE)
    )


def extract_rezconfig_keys(path: Path) -> list[str]:
    """AST-extract top-level simple-name settings from upstream ``rezconfig.py``.

    rez 3.4.0 declares every setting as a module-level ``name = value``
    assignment. Docstrings, imports and expressions are not settings.
    """
    tree = ast.parse(Path(path).read_text(encoding="utf-8"), filename=str(path))
    return [
        target.id
        for node in tree.body
        if isinstance(node, ast.Assign)
        for target in node.targets
        if isinstance(target, ast.Name)
    ]


def extract_schema_keys(path: Path) -> list[str]:
    """AST-extract the ``config_schema`` dict keys from rez-next ``config.py``."""
    tree = ast.parse(Path(path).read_text(encoding="utf-8"), filename=str(path))
    for node in tree.body:
        if isinstance(node, ast.Assign) and any(
            isinstance(target, ast.Name) and target.id == "config_schema" for target in node.targets
        ):
            if not isinstance(node.value, ast.Dict):
                raise ValueError("config_schema is not a dict literal")
            keys = [
                key.value
                for key in node.value.keys
                if isinstance(key, ast.Constant) and isinstance(key.value, str)
            ]
            if len(keys) != len(node.value.keys):
                raise ValueError("config_schema contains a non-string key")
            return keys
    raise ValueError(f"config_schema not found in {path}")


def assert_distinct_sources(schema_path: Path, reference_path: Path) -> None:
    """Reject identical paths or identical contents (a fake empty diff)."""
    schema = Path(schema_path).resolve()
    reference = Path(reference_path).resolve()
    if not schema.is_file() or not reference.is_file():
        raise SourceIndependenceError("both key sources must be existing files")
    if schema == reference:
        raise SourceIndependenceError("schema and reference point at the same file")
    if schema.read_bytes() == reference.read_bytes():
        raise SourceIndependenceError("schema and reference have identical contents")


def assert_independent_sources(schema_path: Path, reference_path: Path) -> None:
    """Require two distinct files from the two different vendored trees.

    The reference must come from the upstream rez 3.4.0 tree and the schema
    from the rez-next tree. rez-next's own ``rezconfig.py`` compatibility
    module is the same side and is rejected as circular evidence.
    """
    assert_distinct_sources(schema_path, reference_path)
    schema = Path(schema_path).resolve()
    reference = Path(reference_path).resolve()
    if not schema.is_relative_to(SCHEMA_TREE.resolve()):
        raise SourceIndependenceError("schema source is not from the rez-next tree")
    if not reference.is_relative_to(REFERENCE_TREE.resolve()):
        raise SourceIndependenceError("reference source is not from the independent rez 3.4.0 tree")


def load_real_key_sources(
    schema_path: Path = SCHEMA_SOURCE,
    reference_path: Path = REFERENCE_SOURCE,
) -> RealKeySources:
    """Read both real key lists, refusing same-source or same-side inputs."""
    assert_independent_sources(schema_path, reference_path)
    return RealKeySources(
        schema_keys=extract_schema_keys(schema_path),
        reference_keys=extract_rezconfig_keys(reference_path),
        schema_path=Path(schema_path),
        reference_path=Path(reference_path),
    )


def load_keys_from_env() -> tuple[list[str], list[str]] | None:
    """Load both env-named key lists, refusing same-file/same-content inputs."""
    schema_path = os.environ.get(SCHEMA_SOURCE_ENV)
    reference_path = os.environ.get(REFERENCE_SOURCE_ENV)
    if not schema_path or not reference_path:
        return None
    assert_distinct_sources(Path(schema_path), Path(reference_path))
    schema = load_keys(SCHEMA_SOURCE_ENV)
    reference = load_keys(REFERENCE_SOURCE_ENV)
    if schema is None or reference is None:
        return None
    return schema, reference
