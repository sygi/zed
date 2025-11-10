#![cfg(feature = "jj-ui")]

mod jj_panel;

use feature_flags::{FeatureFlagAppExt, JjUiFeatureFlag};
use gpui::App;
use workspace::Workspace;

pub fn init(cx: &mut App) {
    if !cx.has_flag::<JjUiFeatureFlag>() {
        return;
    }

    cx.observe_new(|workspace: &mut Workspace, _, _| {
        jj_panel::register(workspace);
    })
    .detach();
}

pub use jj_panel::JjPanel;
pub use jj_panel::JjPanelLoadError;
