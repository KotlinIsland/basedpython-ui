//! `basedpython_ui._native`: the native core of basedpython-ui.
//!
//! The pure-rust modules (`types`, `tree`, `text`, `commit`, `layout`, `paint`, `input`) know
//! nothing about Python and are unit-tested with `cargo test --no-default-features`. The `python`
//! feature adds the pyo3 `Core` class (`py.rs`) and the winit/softbuffer `Window` (`window.rs`).
//! The protocol is `docs/native-protocol.md` in the repository.

pub mod commit;
pub mod input;
pub mod layout;
pub mod paint;
pub mod text;
pub mod tree;
pub mod types;

#[cfg(feature = "python")]
mod py;
#[cfg(feature = "python")]
mod window;

#[cfg(feature = "python")]
#[pyo3::pymodule(gil_used = false)]
fn _native(m: &pyo3::Bound<'_, pyo3::types::PyModule>) -> pyo3::PyResult<()> {
    use pyo3::prelude::PyModuleMethods;
    m.add_class::<py::Core>()?;
    m.add_class::<window::Window>()?;
    m.add("RECORD_INTS", commit::RECORD)?;
    Ok(())
}
