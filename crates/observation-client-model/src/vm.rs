//! View-models: what a panel shows, worked out from the model so the
//! panel only lays it out. Colours, icons and fonts stay with the panel.

mod camera_list;
mod footage;
mod history;
mod loading;
mod status;
mod summary;

pub use camera_list::{CameraListVm, GroupVm, RowVm};
pub use footage::{
    AS_DRAWN_NOTE, AreaRowVm, Cell, ONE_SIDED_NOTE, PlacesVm, SplitRow, SplitRowKind, SplitTableVm,
    SplitVm, TableRow, TargetRowVm, VerdictRung, VerdictVm, WallRow, m2, range_text,
};
pub use history::{HistoryButton, HistoryVm};
pub use loading::LoadingVm;
pub use status::{DemCredit, SaveVm, StatusVm};
pub use summary::{CoverageSummaryVm, Figures, Rung, settings_line};
