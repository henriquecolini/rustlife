//! Errors from the browser, as plain text.

use wasm_bindgen::{JsCast, JsValue};

/// Something the browser refused or failed to do.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct BrowserError(pub String);

impl From<JsValue> for BrowserError {
    fn from(value: JsValue) -> Self {
        Self(describe(&value))
    }
}

/// A readable message for whatever JavaScript threw.
pub(crate) fn describe(value: &JsValue) -> String {
    if let Some(error) = value.dyn_ref::<js_sys::Error>() {
        return format!("{}: {}", error.name(), error.message());
    }
    value.as_string().unwrap_or_else(|| format!("{value:?}"))
}
