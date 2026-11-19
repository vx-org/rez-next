# Rez compatibility baseline

rez-next tracks the public workflow contracts of
[AcademySoftwareFoundation/rez](https://github.com/AcademySoftwareFoundation/rez),
not Rez's private Python module layout. The comparison baseline for this work is
Rez 3.4.0 (`f19dad25ec9cbb6b0e9f53ff2ec5c964b077912a`) plus upstream `main`
(`73a094ddd1628630e96663b7a8de2b66d3edf27f`). The public
`ResolvedContext` method set is unchanged between those two revisions.

## Supported workflow contracts

The following surfaces are intended for production use and are covered by
behavioral tests:

- Local package repositories, requirement solving, and materialized variant
  roots.
- Rex `setenv`, `setenv_if_empty`, `unsetenv`, `resetenv`, prepend, append,
  parent expansion, aliases, and shell activation generation.
- Rez parent environment policies: full inheritance, `parent_variables`, and
  `all_parent_variables`.
- Official `REZ_<PACKAGE>_{ROOT,BASE,VERSION,MAJOR_VERSION,MINOR_VERSION,
  PATCH_VERSION}` metadata and `REZ_USED_*` context metadata.
- Rez-compatible host identifiers, configured weak implicit requests,
  `add_implicit_packages` / `--no-implicit`, and repository-free ephemeral
  range intersection and exclusion.
- Python `ResolvedContext.package_paths`, `get_environ`, `apply`,
  `get_shell_code`, `requested_packages`, `get_resolve_diff`, `get_tools`,
  `get_tool_variants`, `get_conflicting_tools`, and direct `execute_command`
  returning `subprocess.Popen`.
- The standalone `rez-next-runtime` crate used by vx for unpacked bundles: explicit
  target platform/architecture, structured errors, environment export, actual
  roots, and no-shell command spawning.

Paths are handled with `PathBuf`, `std::env::join_paths`, and
`std::env::split_paths`; shell rendering remains platform-specific. Every
behavioral change must retain Windows tests and compile cleanly for the full
workspace so Linux and macOS CI can exercise their native path and shell
branches.

## Known parity gaps

## Console-script compatibility

The official installers place the upstream `rez-*` entry-point names beside
the `rez-next` and `rez` binaries. Implemented names dispatch to the matching
Clap command, `rez-python` starts the host Python interpreter, and `rezolve`
invokes the root `rez` interface. `rez-yaml2py` and `rez-selftest` retain the
upstream spellings while dispatching to `yaml2py` and `self-test`.

`rez-interpret`, `rez-memcache`, `rez-benchmark`, and `rez-pkg-ignore` fail
with an explicit status message because their native implementations are not
available. This keeps scripts from silently selecting an incompatible
operation.

These official Rez surfaces are not yet claimed as fully compatible:

| Surface | Current boundary |
|---|---|
| `ResolvedContext` constructor options | The common packages/paths and `add_implicit_packages` workflows are supported; filters, caching controls, timestamps, and callback options remain to be added. |
| `get_shell_code(..., style=...)` | The argument is accepted; non-default `OutputStyle` rendering is not implemented. |
| `get_actions`, `execute_rex_code`, `execute_shell` | Package activation works, but these public action/subshell APIs remain a later batch. |
| `retargeted`, `get_patched_request` | Context retargeting and patch-lock workflows remain a later batch. |
| `write_to_buffer`, `read_from_buffer`, `from_dict` | File save/load exists; exact official buffer and dictionary schemas remain a later batch. |
| Bundling and package-cache callbacks | CLI bundle/cache workflows exist, but the private `ResolvedContext` callback seams are intentionally not mirrored. |

Compatibility claims should cite the exact upstream baseline and the exercised
workflow. A matching attribute name, mock, or successful serialization alone
is not sufficient evidence.
