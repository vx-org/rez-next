"""Tests for rez_next.plugin_managers module."""

import pytest
from rez_next.plugin_managers import (
    BuildProcessPluginType,
    BuildSystemPluginType,
    CommandPluginType,
    LazySingleton,
    PackageRepositoryPluginType,
    ReleaseHookPluginType,
    ReleaseVCSPluginType,
    RezPluginManager,
    RezPluginType,
    ShellPluginType,
    extend_path,
    plugin_manager,
)


class TestLazySingleton:
    def test_lazy_instantiation(self):
        calls = []

        class MyClass:
            def __init__(self):
                calls.append("init")

        ls = LazySingleton(MyClass)
        assert len(calls) == 0  # not yet instantiated
        instance = ls()
        assert len(calls) == 1
        assert ls() is instance  # same instance returned
        assert len(calls) == 1  # no new instantiation


class TestExtendPath:
    def test_non_list_returns_unchanged(self):
        result = extend_path("string_path", "name")
        assert result == "string_path"

    def test_list_with_missing_dirs(self):
        result = extend_path(["/nonexistent/path"], "some.package")
        assert result == ["/nonexistent/path"]  # unchanged because path missing


class TestRezPluginType:
    def test_type_name_required(self):
        with pytest.raises(TypeError):
            RezPluginType()

    def test_concrete_type(self):
        pt = ShellPluginType()
        assert pt.type_name == "shell"
        assert pt.pretty_type_name == "shell"
        assert isinstance(pt.plugin_classes, dict)
        assert isinstance(pt.failed_plugins, dict)

    def test_get_summary(self):
        summary = plugin_manager.get_summary_string()
        assert isinstance(summary, str)
        assert "shell" in summary or "(no plugins registered)" in summary


class TestPluginTypeSubclasses:
    def test_shell_plugin_type(self):
        pt = ShellPluginType()
        assert pt.type_name == "shell"

    def test_release_vcs_plugin_type(self):
        pt = ReleaseVCSPluginType()
        assert pt.type_name == "release_vcs"

    def test_release_hook_plugin_type(self):
        pt = ReleaseHookPluginType()
        assert pt.type_name == "release_hook"

    def test_build_system_plugin_type(self):
        pt = BuildSystemPluginType()
        assert pt.type_name == "build_system"

    def test_package_repository_plugin_type(self):
        pt = PackageRepositoryPluginType()
        assert pt.type_name == "package_repository"

    def test_build_process_plugin_type(self):
        pt = BuildProcessPluginType()
        assert pt.type_name == "build_process"

    def test_command_plugin_type(self):
        pt = CommandPluginType()
        assert pt.type_name == "command"


class TestPluginManager:
    def test_singleton(self):
        assert plugin_manager is not None
        assert isinstance(plugin_manager, RezPluginManager)

    def test_get_plugin_types(self):
        types = plugin_manager.get_plugin_types()
        assert isinstance(types, list)
        assert "shell" in types
        assert "build_system" in types
        assert "release_vcs" in types
        assert "package_repository" in types
        assert "release_hook" in types
        assert "build_process" in types
        assert "command" in types

    def test_unknown_plugin_type_raises(self):
        from rez_next.exceptions import RezPluginError
        with pytest.raises(RezPluginError):
            plugin_manager.get_plugins("nonexistent")

    def test_unknown_plugin_class_raises(self):
        from rez_next.exceptions import RezPluginError
        with pytest.raises(RezPluginError):
            plugin_manager.get_plugin_class("shell", "nonexistent")

    def test_plugin_path_discovers_package_repository_and_release_hook_plugins(
        self, tmp_path, monkeypatch
    ):
        plugin_root = tmp_path / "plugins"
        release_hook_dir = plugin_root / "rezplugins" / "release_hook"
        package_repository_dir = plugin_root / "rezplugins" / "package_repository"
        release_hook_dir.mkdir(parents=True)
        package_repository_dir.mkdir(parents=True)
        (plugin_root / "rezplugins" / "__init__.py").write_text("")
        (release_hook_dir / "__init__.py").write_text("")
        (package_repository_dir / "__init__.py").write_text("")
        (release_hook_dir / "custom_hook.py").write_text(
            """
from rez_next.release_hook import ReleaseHook


class CustomHook(ReleaseHook):
    @classmethod
    def name(cls):
        return "custom_hook"


def register_plugin():
    return CustomHook
"""
        )
        (package_repository_dir / "custom_repository.py").write_text(
            """
from rez_next.package_repository import PackageRepository


class CustomRepository(PackageRepository):
    @classmethod
    def name(cls):
        return "custom_repository"


def register_plugin():
    return CustomRepository
"""
        )
        (package_repository_dir / "filesystem.py").write_text(
            """
from rez_next.package_repository import PackageRepository


class CustomFilesystem(PackageRepository):
    @classmethod
    def name(cls):
        return "filesystem"


def register_plugin():
    return CustomFilesystem
"""
        )

        from rez_next import config

        monkeypatch.setattr(config, "plugin_path", [str(plugin_root)])
        release_hooks = ReleaseHookPluginType()
        repositories = PackageRepositoryPluginType()

        assert "custom_hook" in release_hooks.plugin_classes
        assert release_hooks.get_plugin_class("custom_hook").name() == "custom_hook"
        assert "custom_repository" in repositories.plugin_classes
        assert (
            repositories.get_plugin_class("custom_repository").name()
            == "custom_repository"
        )
        assert repositories.get_plugin_class("filesystem").__name__ == "CustomFilesystem"
