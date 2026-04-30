"""f3dx-cache: DEPRECATED, consolidated into f3dx[cache] on 2026-04-30.

This package is a transition shim. New code should:

    pip install f3dx[cache]
    from f3dx.cache import Cache, diff, read_jsonl

The transition shim re-exports from `f3dx.cache` so existing imports keep
working while the codebase migrates. This package will be yanked from PyPI
in 4-6 months; old wheels will still resolve from the cache, but new
installs from f3dx-cache will fail by then.

Repo moved: https://github.com/smigolsmigol/f3dx
"""
from __future__ import annotations

import warnings

warnings.warn(
    "f3dx-cache has been consolidated into f3dx. "
    "Install with `pip install f3dx[cache]` and import from `f3dx.cache`. "
    "This transition package will be yanked from PyPI in 4-6 months. "
    "See https://github.com/smigolsmigol/f3dx",
    DeprecationWarning,
    stacklevel=2,
)

from f3dx.cache import Cache, diff, read_jsonl  # noqa: E402

__all__ = ["Cache", "diff", "read_jsonl"]
