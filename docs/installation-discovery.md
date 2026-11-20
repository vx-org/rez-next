# Installation discovery

Tooling and IDE integrations can discover rez-next and an optional legacy Rez
installation without scraping human-readable output:

```console
rez-next info --json
```

The command exits successfully when legacy Rez or Python is absent. Its JSON
object has the following stable fields:

| Field | JSON type | Meaning |
| --- | --- | --- |
| `rez_next_version` | string | Running rez-next version. |
| `rez_legacy_path` | string or null | First legacy `rez` executable found on `PATH`. A rez-next `rez` alias is excluded. |
| `rez_legacy_available` | boolean | Whether `rez_legacy_path` is present. |
| `packages_path` | array of strings | Effective `REZ_PACKAGES_PATH` or configured package paths. |
| `local_packages_path` | string | Effective local package repository. |
| `release_packages_path` | string | Effective release package repository. |
| `config_file` | string or null | First sourced configuration file. |
| `python_version` | string or null | Detected Python version, when Python is available on `PATH`. |

Paths use native platform spelling. In particular, `REZ_PACKAGES_PATH` is
split with `;` on Windows and `:` on Unix platforms. Consumers should ignore
unknown fields so compatible schema additions do not break them.

Example:

```json
{
  "rez_next_version": "0.3.13",
  "rez_legacy_path": null,
  "rez_legacy_available": false,
  "packages_path": ["D:\\packages", "D:\\shared-packages"],
  "local_packages_path": "D:\\packages",
  "release_packages_path": "D:\\release-packages",
  "config_file": "C:\\Users\\developer\\.rezconfig.yaml",
  "python_version": "3.13.7"
}
```

The existing global `rez-next --info` flag remains human-readable and is not a
machine-consumption contract.

For shell scripts that only need an executable path, `rez-next which` prints
the running rez-next executable. `rez-next which --rez` prints the legacy Rez
executable and exits non-zero when legacy Rez is unavailable.
