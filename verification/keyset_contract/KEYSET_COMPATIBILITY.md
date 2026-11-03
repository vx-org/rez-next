# Configuration keyset compatibility policy

`config_schema` covers 140 of the 146 top-level settings in upstream rez 3.4.0
`rezconfig.py`. The reverse diff (`missing_in_reference`) is empty. The six
remaining keys are **not** missing coverage by accident: they are deliberately
excluded by the shipped design, and each exclusion is bound to a production
source justification in `keyset_invariant.py`.

Five are explicitly deprecated rez-1 compatibility settings; one is a plugin
extension namespace. None is a valid setting that rez-next is required to
validate. Adding them to `config_schema` would conflict with the merged design
(a no-op validator for deprecated settings and a flattened dotted namespace),
so the contract keeps them out and pins the exact divergence instead.

## Per-key justification

| Key | Category | Production evidence |
| --- | --- | --- |
| `warn_old_commands` | Deprecated rez-1 setting | Upstream `rezconfig.py` "Rez-1 Compatibility" section, `.. deprecated:: 2.114.0`, "Will be removed in a future release"; listed in `config.py::_deprecated_settings`. |
| `error_old_commands` | Deprecated rez-1 setting | Same deprecated section (companion of `warn_old_commands`), `.. deprecated:: 2.114.0`; listed in `config.py::_deprecated_settings`. |
| `debug_old_commands` | Deprecated rez-1 setting (inert) | Same deprecated section, `.. deprecated:: 2.114.0` and "This currently has no effect"; intentionally not modelled. |
| `rez_1_environment_variables` | Deprecated rez-1 setting | Same deprecated section, `.. deprecated:: 2.114.0` plus `versionchanged:: 3.0.0` (default changed to `False` "in preparation of future removal"); listed in `config.py::_deprecated_settings`. |
| `disable_rez_1_compatibility` | Deprecated rez-1 setting | Same deprecated section, `.. deprecated:: 2.114.0` plus 3.0.0 default changed to `True`; listed in `config.py::_deprecated_settings`; registered in `crates/rez-next-config/src/keys.rs` with `deprecated: true`. |
| `plugins` | Plugin extension namespace | Upstream `plugins = {}` with the "common to all plugins of that type" note; `config.py` handles it separately via `_PluginConfigs` and dotted `plugins.<plugin>.<setting>` paths; `keys.rs` registers it as `KeyType::Dict` with an explicit "registered but not schema-validated as a single value" comment. |

## What the contract rejects

* Any new upstream key not registered in `config_schema` (a seventh diff key).
* Any schema-only key absent from the upstream reference.
* Any deleted or renamed approved key, or a missing per-key justification.
* A same-file, identical-content, same-side or swapped-tree source pair.
* A change to either pinned source blob (four SHA-256 pins: LF-normalized and
  CRLF-rendered forms of each source), or a drift between the vendored pins and
  the live production `config.py` / `keys.rs`.

Deleting keys from either source to force literal equality, reusing one source
for both sides, or adding no-op validators are all rejected rather than treated
as a pass. `run_real_keyset_comparison.py` exits `0` only when the full contract
holds; `test_keyset_invariant.py` genuinely passes with no `expectedFailure`.
