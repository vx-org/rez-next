"""
Release VCS module — aligns with rez.release_vcs.

Provides an abstract version control system (VCS) interface for releasing
Rez packages, along with factory functions for discovering and creating
VCS instances.
"""

from __future__ import annotations

import abc
import os
import subprocess
from typing import Any, ClassVar

from rez_next._native.release import (
    GitVCS as _NativeGitVCS,
)
from rez_next._native.release import (
    MercurialVCS as _NativeMercurialVCS,
)
from rez_next._native.release import (
    SvnVCS as _NativeSvnVCS,
)
from rez_next.exceptions import RezSystemError
from rez_next.packages_ import get_developer_package


class ReleaseVCSError(RezSystemError):
    """Error raised for VCS-related failures in the release process."""


class ReleaseVCS(abc.ABC):
    """Abstract base class for version control system integrations.

    Subclasses must implement all abstract methods to provide VCS support
    for releasing Rez packages (e.g., Git, Mercurial).

    Rez API: ``rez.release_vcs.ReleaseVCS``
    """

    # Registry: maps VCS name → subclass
    _registry: ClassVar[dict[str, type[ReleaseVCS]]] = {}

    def __init__(self, pkg_root: str, vcs_root: str | None = None) -> None:
        """Initialize a VCS instance.

        Args:
            pkg_root: Root directory of the package source.
            vcs_root: Root directory of the VCS repository. If None,
                the VCS implementation should auto-detect it.
        """
        self.pkg_root: str = pkg_root
        self._package = None
        if vcs_root:
            self.vcs_root = vcs_root
        else:
            found = self.find_vcs_root(pkg_root)
            if found is None:
                raise ReleaseVCSError(f"No VCS root found for: {pkg_root}")
            self.vcs_root = found[0]

    @property
    def package(self):
        """The developer package at ``pkg_root`` (lazy-loaded)."""
        if self._package is None:
            try:
                self._package = get_developer_package(self.pkg_root)
            except Exception:
                self._package = None
        return self._package

    # ── Abstract methods ──────────────────────────────────────────────

    @classmethod
    @abc.abstractmethod
    def name(cls) -> str:
        """Return the VCS type name, e.g. ``'git'``."""

    @classmethod
    @abc.abstractmethod
    def is_valid_root(cls, path: str) -> bool:
        """Return ``True`` if *path* is a valid VCS root directory."""

    @classmethod
    @abc.abstractmethod
    def search_parents_for_root(cls) -> bool:
        """Return ``True`` if parent directories should be searched."""

    @classmethod
    def find_vcs_root(cls, path: str) -> tuple[str, int] | None:
        """Find the VCS root directory by walking up parents.

        Returns:
            ``(vcs_root, depth)`` tuple where *depth* is the number of
            parent directories traversed, or ``None`` if not found.
        """
        if not cls.search_parents_for_root():
            if cls.is_valid_root(path):
                return (os.path.abspath(path), 0)
            return None

        current = os.path.abspath(path)
        depth = 0
        while True:
            if cls.is_valid_root(current):
                return (current, depth)
            parent = os.path.dirname(current)
            if parent == current:
                return None
            current = parent
            depth += 1

    @abc.abstractmethod
    def validate_repostate(self) -> None:
        """Ensure the working copy is up-to-date and clean."""

    @abc.abstractmethod
    def get_current_revision(self) -> object:
        """Get the current revision identifier (str, dict, etc.)."""

    @abc.abstractmethod
    def get_changelog(
        self,
        previous_revision: Any = None,
        max_revisions: int | None = None,
    ) -> str:
        """Get changelog text since the given revision."""

    @abc.abstractmethod
    def tag_exists(self, tag_name: str) -> bool:
        """Check if a tag exists in the repository."""

    @abc.abstractmethod
    def create_release_tag(self, tag_name: str, message: str | None = None) -> None:
        """Create a release tag in the repository."""

    @classmethod
    @abc.abstractmethod
    def export(cls, revision: object, path: str) -> None:
        """Export the repository at the given revision to a directory.

        The directory at *path* must not exist (but its parent must).
        """

    # ── Optional helper methods ───────────────────────────────────────

    def get_current_branch(self) -> str | None:
        """Get the current branch name, or ``None`` if in detached HEAD."""
        return None

    def find_executable(self, name: str) -> str:
        """Find a VCS executable by name.

        Raises ``ReleaseVCSError`` if not found.
        """
        exe = ReleaseVCS._which(name)
        if exe is None:
            raise ReleaseVCSError(f"VCS executable not found: {name}")
        return exe

    @staticmethod
    def _which(name: str) -> str | None:
        """Find an executable in PATH."""
        for path_dir in os.environ.get("PATH", "").split(os.pathsep):
            candidate = os.path.join(path_dir, name)
            if os.path.isfile(candidate) and os.access(candidate, os.X_OK):
                return candidate
            # Windows: try with .exe extension
            candidate_exe = candidate + ".exe"
            if os.path.isfile(candidate_exe):
                return candidate_exe
        return None

    def _cmd(self, *args: str) -> list[str]:
        """Run an external command and return output lines.

        Raises ``ReleaseVCSError`` on failure.
        """
        try:
            result = subprocess.run(
                args,
                capture_output=True,
                text=True,
                check=False,
            )
            if result.returncode != 0:
                raise ReleaseVCSError(
                    f"Command failed (exit {result.returncode}): "
                    f"{' '.join(args)}\n{result.stderr.strip()}"
                )
            return [line for line in result.stdout.splitlines() if line]
        except FileNotFoundError:
            raise ReleaseVCSError(f"Executable not found: {args[0]}")
        except OSError as e:
            raise ReleaseVCSError(f"Command error: {e}")

    def __init_subclass__(cls, **kwargs: Any) -> None:
        """Auto-register subclasses in the VCS registry."""
        super().__init_subclass__(**kwargs)
        # Only register concrete subclasses (skip abstract base)
        if not getattr(cls, "__abstractmethods__", None):
            try:
                name = cls.name()
                ReleaseVCS._registry[name] = cls
            except NotImplementedError:
                pass


# ── Factory functions ──────────────────────────────────────────────


def get_release_vcs_types() -> list[str]:
    """Return all registered VCS type names.

    Returns:
        List of VCS names (e.g., ``['git', 'hg', 'svn']``).

    Rez API: ``rez.release_vcs.get_release_vcs_types()``
    """
    return sorted(ReleaseVCS._registry.keys())


def create_release_vcs(path: str, vcs_name: str | None = None) -> ReleaseVCS:
    """Create a ``ReleaseVCS`` instance from a path.

    If *vcs_name* is given, use that specific VCS type. Otherwise,
    auto-detect by checking each registered VCS type.

    Args:
        path: Source package root path.
        vcs_name: Optional specific VCS type name.

    Returns:
        A ``ReleaseVCS`` instance.

    Raises:
        ReleaseVCSError: If no suitable VCS is found.

    Rez API: ``rez.release_vcs.create_release_vcs()``
    """
    if vcs_name:
        cls = ReleaseVCS._registry.get(vcs_name)
        if cls is None:
            raise ReleaseVCSError(f"Unknown VCS type: {vcs_name}")
        return cls(path)

    # Auto-detect: find the closest VCS root
    candidates: list[tuple[str, type[ReleaseVCS], int]] = []
    for name, cls in ReleaseVCS._registry.items():
        try:
            result = cls.find_vcs_root(path)
            if result is not None:
                candidates.append((name, cls, result[1]))
        except Exception:
            continue

    if not candidates:
        raise ReleaseVCSError(
            f"No VCS root found for path: {path}. "
            f"Available types: {list(ReleaseVCS._registry.keys())}"
        )

    # Pick the VCS with the shallowest depth (closest to path)
    candidates.sort(key=lambda x: x[2])
    return candidates[0][1](path)


class _NativeReleaseVCS(ReleaseVCS):
    """Adapt a native release VCS to the Python release VCS contract.

    The Rust implementations own repository operations and expose their
    failures through Python exceptions. This adapter keeps the Python
    registry and root discovery behavior while delegating all repository
    operations to the native implementation.
    """

    _native_class: ClassVar[type]
    _root_marker: ClassVar[str]

    @classmethod
    @abc.abstractmethod
    def name(cls) -> str:
        """Return the registered VCS name."""
        raise NotImplementedError

    @classmethod
    def is_valid_root(cls, path: str) -> bool:
        """Return whether *path* contains this VCS's repository marker."""
        return os.path.exists(os.path.join(os.path.abspath(path), cls._root_marker))

    @classmethod
    def search_parents_for_root(cls) -> bool:
        """Search parent directories for the nearest repository root."""
        return True

    def __init__(self, pkg_root: str, vcs_root: str | None = None) -> None:
        super().__init__(pkg_root, vcs_root)
        try:
            self._native_vcs = self._native_class(self.vcs_root)
        except Exception as exc:
            raise ReleaseVCSError(
                f"Unable to initialize {self.name()} VCS at {self.vcs_root}: {exc}"
            ) from exc

    def _call_native(self, method: str, *args: object) -> Any:
        """Invoke a native VCS operation and preserve the Python error type."""
        try:
            return getattr(self._native_vcs, method)(*args)
        except Exception as exc:
            raise ReleaseVCSError(f"{self.name()} VCS operation failed: {exc}") from exc

    def get_repo_root(self) -> str:
        """Return the repository root selected by the native implementation."""
        return self._call_native("get_repo_root")

    def validate_repostate(self) -> None:
        """Validate the repository state before release."""
        self._call_native("validate_repo_state")

    def get_current_revision(self) -> object:
        """Return the native revision object for the current checkout."""
        return self._call_native("get_current_revision")

    def get_changelog(
        self,
        previous_revision: Any = None,
        max_revisions: int | None = None,
    ) -> str:
        """Return the native changelog for the requested revision range."""
        del max_revisions
        revision_id = None if previous_revision is None else str(previous_revision)
        if hasattr(previous_revision, "revision_id"):
            revision_id = str(previous_revision.revision_id)
        return self._call_native("get_changelog", revision_id, None)

    def tag_exists(self, tag_name: str) -> bool:
        """Return whether a release tag exists."""
        return self._call_native("tag_exists", tag_name)

    def create_release_tag(self, tag_name: str, message: str | None = None) -> None:
        """Create a release tag using the native VCS implementation."""
        self._call_native("create_tag", tag_name, message or "")

    def create_tag(self, tag_name: str, message: str | None = None) -> None:
        """Provide the shorter method name used by the build process."""
        self.create_release_tag(tag_name, message)

    def get_current_branch(self) -> str | None:
        """Return the current branch when the native VCS exposes one."""
        return self._call_native("get_current_branch")

    def is_releasable_branch(self) -> bool | None:
        """Return whether the current branch is suitable for release."""
        return self._call_native("is_releasable_branch")

    def export(self, revision: object, path: str) -> None:
        """Export a native revision through the initialized VCS instance.

        Rust's current VCS trait binds export to an instance repository. The
        Python contract is reconciled in issue #350, while this adapter keeps
        the already-implemented native operation usable for registry clients.
        """
        self._call_native("export", revision, path)


class GitVCS(_NativeReleaseVCS):
    """Git release VCS backed by the native Rust implementation."""

    _native_class = _NativeGitVCS
    _root_marker = ".git"

    @classmethod
    def name(cls) -> str:
        return "git"


class MercurialVCS(_NativeReleaseVCS):
    """Mercurial release VCS backed by the native Rust implementation."""

    _native_class = _NativeMercurialVCS
    _root_marker = ".hg"

    @classmethod
    def name(cls) -> str:
        return "hg"


class SvnVCS(_NativeReleaseVCS):
    """SVN release VCS backed by the native Rust implementation."""

    _native_class = _NativeSvnVCS
    _root_marker = ".svn"

    @classmethod
    def name(cls) -> str:
        return "svn"
