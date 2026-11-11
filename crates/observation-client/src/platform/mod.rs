//! Everything that differs between the browser and the native build: the
//! browser glue (`web_sys`, `js_sys`, `wasm_bindgen`) in `web.rs` and its
//! native stand-ins in `native.rs`, with the same free functions on both.
//! This is the only module of the library that tests `target_arch`.

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(target_arch = "wasm32")]
mod web;

#[cfg(not(target_arch = "wasm32"))]
pub use native::*;
#[cfg(target_arch = "wasm32")]
pub use web::*;

/// One HTTP request.
#[derive(Debug, Clone)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    allow(
        dead_code,
        reason = "only the browser build's fetch reads more than the URL"
    )
)]
pub struct HttpRequest {
    pub url: String,
    pub method: &'static str,
    /// JSON body.
    pub body: Option<String>,
    pub bearer: Option<String>,
}

impl HttpRequest {
    pub fn get(url: String) -> Self {
        Self {
            url,
            method: "GET",
            body: None,
            bearer: None,
        }
    }
}

/// Why the browser refused the autosave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    allow(dead_code, reason = "only the browser build's storage writes fail")
)]
pub enum SaveFailure {
    /// The site's storage quota is used up (or is zero, as in some private modes).
    Full,
    /// The browser denies this page access to storage (site data or cookies blocked).
    Blocked,
    /// The browser offers no storage to this page at all.
    Unavailable,
}

impl SaveFailure {
    /// What went wrong, for the status bar's tooltip.
    pub fn reason(self) -> &'static str {
        match self {
            Self::Full => "This browser's storage for the site is full.",
            Self::Blocked => {
                "This browser blocks the site from storing data (check its site data or cookie settings)."
            }
            Self::Unavailable => {
                "This browser offers no storage to the site (it may be in private mode)."
            }
        }
    }
}
