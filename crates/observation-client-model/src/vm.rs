//! View-models: what a panel shows, worked out from the model so the
//! panel only lays it out. Colours, icons and fonts stay with the panel.

mod loading;
mod status;

pub use loading::LoadingVm;
pub use status::{DemCredit, SaveVm, StatusVm};
