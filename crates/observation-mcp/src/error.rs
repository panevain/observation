//! Why a tool call failed, worded for the agent that made it.

use observation_core::scene::{SceneError, SceneFile};
use thiserror::Error;

/// A failed tool call. The server returns it as a tool result with
/// `isError` set, so the agent sees the message and can correct its call.
#[derive(Debug, Error)]
pub enum ToolError {
    #[error(transparent)]
    Scene(#[from] SceneError),
    /// The id asked for, and the ids there are ([`ToolError::no_camera`]).
    #[error("no camera with id {0} in the scene; {1}")]
    NoCamera(u32, String),
    #[error(
        "no camera model `{0}` in the catalogue or the scene's custom models: call \
         list_camera_models for the ids"
    )]
    NoModel(String),
    #[error("no custom model `{0}` in the scene")]
    NoCustomModel(String),
    #[error(
        "this needs the area's buildings and terrain, which are only available for the demo street \
         unless the Observation app is connected (connect_app); the scene is on an OpenStreetMap area"
    )]
    AreaUnavailable,
    #[error("the connected app's server could not send the area: {0}")]
    AreaFetch(String),
    #[error("the demo street could not be built: {0}")]
    Demo(String),
    #[error("the camera catalogue could not be loaded: {0}")]
    Catalog(String),
    #[error(
        "no app is connected: ask the user for the code in the app's Connect agent dialog, then call connect_app"
    )]
    NotConnected,
    #[error("{0}")]
    Link(String),
    #[error("{0}")]
    Invalid(String),
}

impl ToolError {
    pub(crate) fn invalid(why: impl Into<String>) -> Self {
        Self::Invalid(why.into())
    }

    /// No camera `id` in `scene`, saying which cameras there are.
    pub(crate) fn no_camera(scene: &SceneFile, id: u32) -> Self {
        Self::NoCamera(
            id,
            ids_there(
                "cameras",
                scene.cameras.iter().map(|c| c.id),
                "place_camera",
            ),
        )
    }
}

/// "its cameras are 1, 2 and 4", or "it has no cameras yet (place_camera
/// adds one)": what an agent that named a missing item can use instead.
pub(crate) fn ids_there(what: &str, ids: impl Iterator<Item = u32>, adder: &str) -> String {
    let ids: Vec<String> = ids.map(|i| i.to_string()).collect();
    match ids.as_slice() {
        [] => format!("it has no {what} yet ({adder} adds one)"),
        [one] => format!("its only one has id {one}"),
        [rest @ .., last] => format!("its {what} are {} and {last}", rest.join(", ")),
    }
}
