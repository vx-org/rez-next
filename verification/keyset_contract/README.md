# Configuration keyset contract verification

This directory pins the invariant that the `config_schema` key set in
`crates/rez-next-python/python/rez_next/config.py` stays consistent with the
upstream rez 3.4.0 `src/rez/rezconfig.py` key set.

Two **independent**, hash-pinned sources are vendored read-only under
`upstream/`:

| Side | File | SHA-256 prefix (LF-normalized) | Keys | Origin |
| --- | --- | --- | --- | --- |
| Reference | `upstream/rez-3.4.0/src/rez/rezconfig.py` | `0bf79731e179` | 146 | upstream rez 3.4.0 |
| Schema | `upstream/rez-next@2405006/.../python/rez_next/config.py` (`config_schema`) | `eb812d59fed0` | 140 | config schema delivery |

Both come from different trees, are byte-different, and are AST-extracted
independently (top-level `Name` assignments vs. the `config_schema` dict
literal). Pointing both inputs at one file, or using rez-next's own
`rezconfig.py` compatibility module as the reference, is rejected by
`assert_independent_sources()`. The pinned schema and `keys.rs` blobs are also
verified byte-for-byte against the live production files, so the pins cannot
drift away from what the repository ships.

## Accepted invariant

Strict set equality is **not** the acceptance criterion. The six upstream keys
listed below are deliberately unsupported by `config_schema` per the shipped
design. The accepted, contract-bounded invariant is:

* `missing_in_reference` (schema keys absent upstream) is **empty**;
* `missing_in_schema` is **exactly** the six approved keys, each with a
  production source justification bound in `keyset_invariant.py`.

Any new, removed or renamed key, any non-independent source, a tampered pinned
blob, or a missing per-key justification is a failure. Nothing is deleted from
either source, no same-side source is reused, and no no-op validator is added
to manufacture equality. See `KEYSET_COMPATIBILITY.md` for the per-key policy.

## Files

| File | Purpose |
| --- | --- |
| `keyset_invariant.py` | AST extraction, source-independence checks, four source pins, approved divergence, per-key classifications, contract predicate |
| `test_keyset_invariant.py` | Comparator, independence, hash-pin, contract and per-key classification tests |
| `run_real_keyset_comparison.py` | Standalone real comparison; exits `0` when the contract holds, `1` otherwise |
| `mutation_checks.py` | Applies each negative mutation to a private copy and requires the named test to fail |
| `upstream/` | Read-only, pinned independent sources and production evidence |

## How to run

From the repository root:

```bash
python -I verification/keyset_contract/run_real_keyset_comparison.py
python -m unittest discover -s verification/keyset_contract -v
python verification/keyset_contract/mutation_checks.py
```

All three are self-contained: no network, no install, no credentials.
`mutation_checks.py` runs entirely in private temporary copies and never
modifies the repository. Set `KEYSET_CONTRACT_REPO_ROOT` when running from a
copy so the production-binding checks still read the shipped files.
