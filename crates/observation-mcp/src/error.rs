//! Why a tool call failed, worded for the agent that made it.

use observation_core::scene::SceneError;
use thiserror::Error;

/// A failed tool call. The server returns it as a tool result with
/// `isError` set, so the agent sees the message and can correct its call.
#[derive(Debug, Error)]
pub enum ToolError {
    #[error(transparent)]
    Scene(#[from] SceneError),
    #[error("no camera with id {0} in the scene")]
    NoCamera(u32),
    #[error("no camera model `{0}` in the catalogue or the scene's custom models")]
    NoModel(String),
    #[error("no custom model `{0}` in the scene")]
    NoCustomModel(String),
    #[error(
        "this needs the area's buildings and terrain, which are only available for the demo street; \
         the scene is on an OpenStreetMap area"
    )]
    AreaUnavailable,
    #[error("the demo street could not be built: {0}")]
    Demo(String),
    #[error("the camera catalogue could not be loaded: {0}")]
    Catalog(String),
    #[error("{0}")]
    Invalid(String),
}

impl ToolError {
    pub(crate) fn invalid(why: impl Into<String>) -> Self {
        Self::Invalid(why.into())
    }
}
