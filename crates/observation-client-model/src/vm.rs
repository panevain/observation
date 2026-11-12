//! View-models: what a panel shows, worked out from the model so the
//! panel only lays it out. Colours, icons and fonts stay with the panel.

mod camera_list;
mod loading;
mod status;
mod summary;

pub use camera_list::{CameraListVm, GroupVm, RowVm};
pub use loading::LoadingVm;
pub use status::{DemCredit, SaveVm, StatusVm};
pub use summary::{CoverageSummaryVm, Figures, Rung, settings_line};
