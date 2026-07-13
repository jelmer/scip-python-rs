# scip-python

A [SCIP](https://github.com/sourcegraph/scip) indexer for Python, written in
Rust on top of ruff's
[ruff_python_parser](https://crates.io/crates/ruff_python_parser).

Name resolution happens in two stages. A syntactic pass does scope-based
resolution of everything that doesn't need types: definitions, imports,
locals, `self` attributes. A second pass then resolves the remaining
attribute references (`obj.method()` and the like) through
[ty](https://github.com/astral-sh/ty)'s type inference engine
(`ty_python_semantic`). The inference pass can be disabled with
`--no-infer`, which makes indexing roughly an order of magnitude faster at
the cost of losing type-dependent references.

## Usage

```console
$ scip-python [DIR] [--project-name NAME] [--project-version VERSION] [-o index.scip]
```

The project name and version default to the `[project]` table in
`pyproject.toml` (or `[tool.poetry]`), falling back to the directory name.
When built with the `pyo3` feature, metadata that pyproject.toml does not
state statically -- dynamic versions in particular -- is obtained by asking
the project's PEP 517 build backend, using the Python environment
scip-python runs in. The resulting `index.scip` can be uploaded to
Sourcegraph or consumed by any other SCIP tooling.

## What gets indexed

- module, class, function, method and parameter definitions, with
  docstrings attached to the emitted symbol information
- module- and class-level variable assignments
- instance attributes assigned via `self.attr` in methods
- local variables, emitted as SCIP local symbols
- imports, including relative imports, aliases and re-exports through
  `__init__.py`; cross-module references resolve to the defining module
- references to external modules, using a synthesized package with version
  `unknown`

Files under directories named `venv`, `node_modules`, `build`, `dist`,
`__pycache__`, `*.egg-info` or starting with a dot are skipped. A top-level
`src/` directory is treated as a source root rather than a package.

## Limitations

- no type inference: `obj.method()` only resolves when `obj` is a module
  or `self`
- star re-exports (`from x import *` in `__init__.py`) are not expanded
  into the export table
- PEP 695 type parameters are not yet bound
- files that fail to parse are reported on stderr and skipped

## Development

The ruff and ty crates are vendored as a git submodule (pinned to the
commit their crates.io releases were built from, since upstream treats
them as internal crates without stability guarantees):

```console
$ git submodule update --init --depth 1
$ cargo test
```
