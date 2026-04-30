# f3dx-cache (DEPRECATED)

> **This package has moved.** As of 2026-04-30, `f3dx-cache` is consolidated into `f3dx` as a Python sub-module + Cargo workspace member. Install the new home and update your imports.

```bash
pip install f3dx[cache]
```

```python
from f3dx.cache import Cache, diff, read_jsonl
```

## Why moved

Single wheel, single ABI, single CI, single release cadence. The cache is one of f3dx's runtime layers (alongside the agent runtime, HTTP clients, router, MCP, trace sink); shipping it as a separate package created cross-repo version drift and discoverability friction. See the consolidation plan and architectural reasoning at [smigolsmigol/f3dx](https://github.com/smigolsmigol/f3dx).

## Transition timeline

- **v0.0.4** (this version, 2026-04-30): re-exports from `f3dx.cache`, emits `DeprecationWarning` on import. Install pulls in `f3dx>=0.0.18` automatically.
- **+4 weeks** (2026-05-28): this GitHub repo flips to read-only / archived. PyPI installs of `f3dx-cache==0.0.4` continue to work.
- **+4-6 months** (2026-08 to 2026-10): all `f3dx-cache` versions on PyPI get yanked. Cached wheels still resolve for old installs; new `pip install f3dx-cache` will fail by then.

## What you get with this version

```python
import f3dx_cache  # DeprecationWarning fires here

cache = f3dx_cache.Cache("/path/to/cache.redb")
# Identical to: from f3dx.cache import Cache; cache = Cache("...")
```

The full surface (`Cache`, `diff`, `read_jsonl`) is preserved. The shim is one import statement; performance is identical to importing from `f3dx.cache` directly.

## Migration

Find/replace across your codebase:

| Old | New |
|---|---|
| `pip install f3dx-cache` | `pip install f3dx[cache]` |
| `from f3dx_cache import Cache` | `from f3dx.cache import Cache` |
| `from f3dx_cache import diff` | `from f3dx.cache import diff` |
| `from f3dx_cache import read_jsonl` | `from f3dx.cache import read_jsonl` |

## License

MIT, same as the upstream f3dx project.
