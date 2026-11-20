"""
Allow running `python -m rez_next` for basic info.
"""

from __future__ import annotations

import sys


def main() -> None:
    """Print basic information about rez_next."""
    from rez_next import __author__, __license__, __version__

    if len(sys.argv) > 1:
        try:
            from rez_next._native.cli import cli_main

            raise SystemExit(cli_main(sys.argv[1:]))
        except SystemExit:
            raise
        except Exception as error:
            raise SystemExit(f"rez_next CLI dispatch failed: {error}") from error

    print(f"rez_next version {__version__}")
    print(f"Author: {__author__}")
    print(f"License: {__license__}")
    print("\nThis is a high-performance Rust rewrite of Rez.")
    print("Use `import rez_next as rez` for Rez-compatible API.")


if __name__ == "__main__":
    main()
