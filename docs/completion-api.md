# Completion API

rez-next exposes one completion surface for IDE/LSP clients and interactive
shells. Queries use the configured repositories, including
`REZ_PACKAGES_PATH`, and do not require Python or a legacy Rez installation.

## Machine-readable queries

Query package families by name prefix:

```bash
rez-next completions packages --prefix pyth --json
```

Schema version 1:

```json
{
  "schema_version": 1,
  "kind": "packages",
  "packages": [
    {
      "name": "python",
      "versions": ["3.11.0", "3.10.0", "3.9.0"],
      "description": "Python interpreter"
    }
  ]
}
```

Query versions for one package family:

```bash
rez-next completions versions --package python --json
```

Schema version 1:

```json
{
  "schema_version": 1,
  "kind": "versions",
  "package": "python",
  "versions": ["3.11.0", "3.10.0", "3.9.0"]
}
```

The following fields are stable within schema version 1:

- `schema_version` is the integer `1`.
- `kind` is `packages` or `versions` and identifies the response shape.
- Package results are ordered by name. Each family appears once.
- Versions are unique and use Rez's latest-first ordering.
- `description` is a string or `null`.
- A successful query with no matches returns an empty array.

Clients must reject unknown schema versions instead of guessing their shape.
Future backward-incompatible response changes will increment
`schema_version`.

Use repeatable `--repository PATH` arguments to replace configured repository
paths for a query. Without them, rez-next loads its normal configuration and
honours `REZ_PACKAGES_PATH`.

## Dynamic shell completions

Generate a registration script for any supported shell:

```bash
rez-next completions shell --shell bash
rez-next completions shell --shell zsh
rez-next completions shell --shell fish
rez-next completions shell --shell powershell
```

The registration delegates completion back to the installed executable. This
keeps subcommands, flags, package families, and package versions aligned with
the current rez-next command tree and repositories.

The official install scripts install these registrations automatically. Set
`REZ_NEXT_NO_COMPLETIONS=1` before installation to opt out. The legacy
`rez-next complete` command remains available for compatibility with existing
static completion workflows.
