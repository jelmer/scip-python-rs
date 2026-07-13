use std::path::Path;

use anyhow::{Context as _, Result};

#[derive(Debug, Default, PartialEq)]
pub struct ProjectMetadata {
    pub name: Option<String>,
    pub version: Option<String>,
}

/// Determine the project name and version for a source tree. Reads static
/// metadata from pyproject.toml ([project], then [tool.poetry]); with the
/// pyo3 feature enabled, anything still missing is requested from the
/// project's PEP 517 build backend, which handles dynamic versions.
pub fn project_metadata(dir: &Path) -> Result<ProjectMetadata> {
    let metadata = static_metadata(dir)?;
    #[cfg(feature = "pyo3")]
    let metadata = {
        let mut metadata = metadata;
        if (metadata.name.is_none() || metadata.version.is_none())
            && (dir.join("pyproject.toml").exists() || dir.join("setup.py").exists())
        {
            match build_backend_metadata(dir) {
                Ok(backend) => {
                    metadata.name = metadata.name.or(backend.name);
                    metadata.version = metadata.version.or(backend.version);
                }
                Err(err) => {
                    eprintln!("warning: cannot query build backend for metadata: {err:#}");
                }
            }
        }
        metadata
    };
    Ok(metadata)
}

fn static_metadata(dir: &Path) -> Result<ProjectMetadata> {
    let path = dir.join("pyproject.toml");
    if !path.exists() {
        return Ok(ProjectMetadata::default());
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    let pyproject = pyproject_toml::PyProjectToml::new(&text)
        .with_context(|| format!("cannot parse {}", path.display()))?;
    let mut name = pyproject.project.as_ref().map(|p| p.name.clone());
    let mut version = pyproject
        .project
        .as_ref()
        .and_then(|p| p.version.as_ref())
        .map(|v| v.to_string());
    if name.is_none() || version.is_none() {
        // Poetry projects keep their metadata under [tool.poetry], which
        // the pyproject-toml crate does not model.
        let value: toml::Table = text
            .parse()
            .with_context(|| format!("cannot parse {}", path.display()))?;
        let poetry = value.get("tool").and_then(|tool| tool.get("poetry"));
        let get = |key: &str| {
            poetry
                .and_then(|table| table.get(key))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        };
        name = name.or_else(|| get("name"));
        version = version.or_else(|| get("version"));
    }
    Ok(ProjectMetadata { name, version })
}

/// Ask the project's PEP 517 build backend to prepare metadata and read the
/// resulting name and version. Runs the backend in the current Python
/// environment, without build isolation.
#[cfg(feature = "pyo3")]
fn build_backend_metadata(dir: &Path) -> Result<ProjectMetadata> {
    use anyhow::anyhow;
    use pyo3::prelude::*;

    const PROBE: &std::ffi::CStr = cr#"
import contextlib
import email.parser
import importlib
import os
import sys
import tempfile
import tomllib


def project_metadata(path):
    # Build backends print progress to stdout; keep stdout clean.
    with contextlib.redirect_stdout(sys.stderr):
        return _project_metadata(path)


def _project_metadata(path):
    backend_name = "setuptools.build_meta:__legacy__"
    backend_path = None
    pyproject = os.path.join(path, "pyproject.toml")
    if os.path.exists(pyproject):
        with open(pyproject, "rb") as f:
            data = tomllib.load(f)
        build_system = data.get("build-system", {})
        backend_name = build_system.get("build-backend", backend_name)
        backend_path = build_system.get("backend-path")
    if backend_path:
        sys.path[:0] = [os.path.join(path, p) for p in backend_path]
    module_name, _, attrs = backend_name.partition(":")
    backend = importlib.import_module(module_name)
    for attr in attrs.split("."):
        if attr:
            backend = getattr(backend, attr)
    cwd = os.getcwd()
    os.chdir(path)
    try:
        with tempfile.TemporaryDirectory() as tmp:
            dist_info = backend.prepare_metadata_for_build_wheel(tmp)
            with open(os.path.join(tmp, dist_info, "METADATA")) as f:
                message = email.parser.Parser().parse(f, headersonly=True)
            return message.get("Name"), message.get("Version")
    finally:
        os.chdir(cwd)
"#;

    let dir = dir
        .to_str()
        .ok_or_else(|| anyhow!("project root {} is not valid UTF-8", dir.display()))?;
    let (name, version) = Python::attach(|py| -> PyResult<(Option<String>, Option<String>)> {
        let module = PyModule::from_code(py, PROBE, c"metadata_probe.py", c"metadata_probe")?;
        module.getattr("project_metadata")?.call1((dir,))?.extract()
    })
    .map_err(|err| anyhow!("build backend failed: {err}"))?;
    Ok(ProjectMetadata { name, version })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    #[test]
    fn pep621_metadata() {
        assert_eq!(
            static_metadata(&fixture("meta-pep621")).unwrap(),
            ProjectMetadata {
                name: Some("pep621-project".to_string()),
                version: Some("2.3".to_string()),
            }
        );
    }

    #[test]
    fn poetry_metadata() {
        assert_eq!(
            static_metadata(&fixture("meta-poetry")).unwrap(),
            ProjectMetadata {
                name: Some("poetry-project".to_string()),
                version: Some("4.5".to_string()),
            }
        );
    }

    #[test]
    fn missing_metadata() {
        assert_eq!(
            static_metadata(&fixture("simple")).unwrap(),
            ProjectMetadata::default()
        );
    }
}
