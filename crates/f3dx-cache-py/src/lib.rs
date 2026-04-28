//! PyO3 bridge for f3dx-cache + f3dx-replay.
//!
//! Python surface:
//!   f3dx_cache.Cache(path)      open / create a cache file
//!   cache.get(req)              -> Optional[bytes]
//!   cache.put(req, resp, **meta) -> str (fingerprint hex)
//!   cache.fingerprint(req)      -> str (hex; pure function, no side-effect)
//!   cache.stats()               -> dict
//!   f3dx_cache.diff(a, b, mode) -> tuple[bool, Optional[str]]
//!   f3dx_cache.read_jsonl(path) -> list[dict]

use f3dx_cache_core::{Cache as CoreCache, CachedMeta};
use f3dx_replay::{DiffMode, diff as core_diff, read_jsonl as core_read_jsonl};
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};
use std::sync::Mutex;

#[pyclass(name = "Cache", module = "f3dx_cache")]
struct PyCache {
    inner: Mutex<CoreCache>,
}

#[pymethods]
impl PyCache {
    #[new]
    fn new(path: String) -> PyResult<Self> {
        let cache = CoreCache::open(path).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
        Ok(Self {
            inner: Mutex::new(cache),
        })
    }

    fn fingerprint(&self, request_json: &str) -> PyResult<String> {
        let value: serde_json::Value =
            serde_json::from_str(request_json).map_err(|e| PyValueError::new_err(e.to_string()))?;
        let cache = self.inner.lock().expect("cache mutex poisoned");
        cache
            .fingerprint(&value)
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))
    }

    #[pyo3(signature = (request_json, response, model = None, system_fingerprint = None, response_duration_ms = None))]
    fn put(
        &self,
        request_json: &str,
        response: &[u8],
        model: Option<String>,
        system_fingerprint: Option<String>,
        response_duration_ms: Option<u64>,
    ) -> PyResult<String> {
        let value: serde_json::Value =
            serde_json::from_str(request_json).map_err(|e| PyValueError::new_err(e.to_string()))?;
        let meta = CachedMeta {
            created_at_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            hit_count: 0,
            model,
            system_fingerprint,
            response_duration_ms,
        };
        let cache = self.inner.lock().expect("cache mutex poisoned");
        cache
            .put(&value, response, &meta)
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))
    }

    fn get<'py>(
        &self,
        py: Python<'py>,
        request_json: &str,
    ) -> PyResult<Option<Bound<'py, PyBytes>>> {
        let value: serde_json::Value =
            serde_json::from_str(request_json).map_err(|e| PyValueError::new_err(e.to_string()))?;
        let cache = self.inner.lock().expect("cache mutex poisoned");
        let res = cache
            .get(&value)
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
        Ok(res.map(|b| PyBytes::new(py, &b)))
    }

    /// Read-only lookup: skips the hit-count bump for sub-100us warm hits.
    /// Use this when stats accuracy is not needed - typical case is CI
    /// replay against a captured trace, where the cardinality of cache
    /// hits is already known.
    fn peek<'py>(
        &self,
        py: Python<'py>,
        request_json: &str,
    ) -> PyResult<Option<Bound<'py, PyBytes>>> {
        let value: serde_json::Value =
            serde_json::from_str(request_json).map_err(|e| PyValueError::new_err(e.to_string()))?;
        let cache = self.inner.lock().expect("cache mutex poisoned");
        let res = cache
            .peek(&value)
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
        Ok(res.map(|b| PyBytes::new(py, &b)))
    }

    fn stats<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let cache = self.inner.lock().expect("cache mutex poisoned");
        let s = cache
            .stats()
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
        let out = PyDict::new(py);
        out.set_item("entries", s.entries)?;
        out.set_item("hits", s.hits)?;
        out.set_item("misses", s.misses)?;
        out.set_item("bytes_requests", s.bytes_requests)?;
        out.set_item("bytes_responses", s.bytes_responses)?;
        Ok(out)
    }
}

fn parse_mode(mode: &str) -> PyResult<DiffMode> {
    match mode {
        "bytes" => Ok(DiffMode::Bytes),
        "structured" => Ok(DiffMode::Structured),
        "embedding" => Ok(DiffMode::Embedding),
        "judge" => Ok(DiffMode::Judge),
        other => Err(PyValueError::new_err(format!(
            "unknown diff mode {other:?}; expected bytes|structured|embedding|judge"
        ))),
    }
}

#[pyfunction]
#[pyo3(signature = (before, after, mode = "structured".to_string()))]
fn diff(before: &str, after: &str, mode: String) -> PyResult<(bool, Option<String>)> {
    let m = parse_mode(&mode)?;
    Ok(core_diff(before, after, m))
}

#[pyfunction]
fn read_jsonl<'py>(py: Python<'py>, path: String) -> PyResult<Vec<Bound<'py, PyDict>>> {
    let rows = core_read_jsonl(&path).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let json =
            serde_json::to_value(&row).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
        let d = PyDict::new(py);
        if let serde_json::Value::Object(map) = json {
            for (k, v) in map {
                d.set_item(k, json_to_pyobject(py, &v)?)?;
            }
        }
        out.push(d);
    }
    Ok(out)
}

fn json_to_pyobject<'py>(
    py: Python<'py>,
    v: &serde_json::Value,
) -> PyResult<pyo3::Bound<'py, pyo3::PyAny>> {
    use pyo3::IntoPyObject;
    use pyo3::types::{PyList, PyNone};
    match v {
        serde_json::Value::Null => Ok(PyNone::get(py).to_owned().into_any()),
        serde_json::Value::Bool(b) => Ok(b.into_pyobject(py)?.to_owned().into_any()),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(i.into_pyobject(py)?.into_any())
            } else if let Some(f) = n.as_f64() {
                Ok(f.into_pyobject(py)?.into_any())
            } else {
                Ok(n.to_string().into_pyobject(py)?.into_any())
            }
        }
        serde_json::Value::String(s) => Ok(s.into_pyobject(py)?.into_any()),
        serde_json::Value::Array(arr) => {
            let list = PyList::empty(py);
            for item in arr {
                list.append(json_to_pyobject(py, item)?)?;
            }
            Ok(list.into_any())
        }
        serde_json::Value::Object(map) => {
            let d = PyDict::new(py);
            for (k, val) in map {
                d.set_item(k, json_to_pyobject(py, val)?)?;
            }
            Ok(d.into_any())
        }
    }
}

#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyCache>()?;
    m.add_function(wrap_pyfunction!(diff, m)?)?;
    m.add_function(wrap_pyfunction!(read_jsonl, m)?)?;
    Ok(())
}
