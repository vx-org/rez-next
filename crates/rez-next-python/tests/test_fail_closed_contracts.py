"""Regression tests for public compatibility surfaces."""

import pytest
from rez_next.package_copy import copy_package
from rez_next.package_move import move_package
from rez_next.resolver import Resolver


def _write_package(root, name="tool", version="1.0.0"):
    package_dir = root / name / version
    package_dir.mkdir(parents=True)
    (package_dir / "package.py").write_text(
        f"name = '{name}'\nversion = '{version}'\n",
        encoding="utf-8",
    )
    (package_dir / "payload.txt").write_text("payload", encoding="utf-8")
    return package_dir


def test_copy_supports_dry_run(tmp_path):
    source = tmp_path / "source"
    destination = tmp_path / "destination"
    _write_package(source)

    result = copy_package(
        "tool",
        str(destination),
        dry_run=True,
        package_repos=[source],
    )

    assert result["copied"]
    assert not destination.exists()


def test_copy_supports_destination_rename(tmp_path):
    source = tmp_path / "source"
    destination = tmp_path / "destination"
    _write_package(source)

    copy_package(
        "tool",
        str(destination),
        dest_name="renamed",
        package_repos=[source],
    )

    assert (destination / "renamed" / "1.0.0" / "package.py").is_file()


def test_move_supports_timestamp_preservation(tmp_path):
    source = tmp_path / "source"
    destination = tmp_path / "destination"
    _write_package(source)

    move_package(
        "tool",
        str(destination),
        keep_timestamp=True,
        package_repos=[source],
    )

    assert (destination / "tool" / "1.0.0" / "package.py").is_file()
    assert not (source / "tool" / "1.0.0").exists()


def test_resolver_defaults_to_uncached_execution():
    resolver = Resolver(None, [], [])
    assert resolver.caching is False


def test_resolver_rejects_unimplemented_cache_request():
    with pytest.raises(NotImplementedError, match="caching"):
        Resolver(None, [], [], caching=True)
