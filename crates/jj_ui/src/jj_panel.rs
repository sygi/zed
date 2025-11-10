use anyhow::{Result, anyhow};
use feature_flags::{FeatureFlagAppExt as _, JjUiFeatureFlag};
use gpui::{
    App, AsyncWindowContext, Context, Entity, EventEmitter, FocusHandle, Focusable, SharedString,
    Task, WeakEntity, Window, actions,
};
use project::{JjCommitSummary, Project};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use ui::{AnyElement, prelude::*};
use workspace::{DockPosition, Panel, PanelEvent, Workspace};

actions!(jj_panel_actions, [ToggleFocus]);

pub fn register(workspace: &mut Workspace) {
    workspace.register_action(|workspace, _: &ToggleFocus, window, cx| {
        workspace.toggle_panel_focus::<JjPanel>(window, cx);
    });
}

pub struct JjPanel {
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    focus_handle: FocusHandle,
    commits: Vec<JjCommitSummary>,
    is_loading: bool,
    error: Option<SharedString>,
    _task: Option<Task<()>>,
}

impl JjPanel {
    pub fn new(
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<Self> {
        let project = workspace.project().clone();
        let focus_handle = cx.focus_handle();
        cx.on_focus(&focus_handle, window, Self::focus_in).detach();
        let panel_workspace = workspace.downgrade();
        cx.new(|cx| {
            let mut panel = Self {
                workspace: panel_workspace,
                project,
                focus_handle,
                commits: Vec::new(),
                is_loading: true,
                error: None,
                _task: None,
            };
            panel.request_refresh(window, cx);
            panel
        })
    }

    pub async fn load(
        workspace: WeakEntity<Workspace>,
        mut cx: AsyncWindowContext,
    ) -> Result<Entity<Self>> {
        if !cx.has_flag::<JjUiFeatureFlag>() {
            return Err(anyhow!("jj-ui flag is disabled"));
        }
        workspace.update_in(&mut cx, |workspace, window, cx| {
            Ok(Self::new(workspace, window, cx))
        })?
    }

    fn request_refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(jj_store) = self.project.read(cx).jj_store().cloned() else {
            self.error = Some("JJ support unavailable".into());
            self.is_loading = false;
            cx.notify();
            return;
        };
        self.is_loading = true;
        self.error = None;
        cx.notify();
        if let Some(task) = jj_store.update(cx, |store, cx| store.recent_commits(50, cx)) {
            let panel = cx.weak_entity();
            self._task = Some(cx.spawn_in(window, async move |_, cx| match task.await {
                Ok(commits) => {
                    if let Some(panel) = panel.upgrade() {
                        panel
                            .update(cx, |panel, cx| {
                                panel.commits = commits;
                                panel.is_loading = false;
                                panel.error = None;
                                cx.notify();
                            })
                            .ok();
                    }
                }
                Err(err) => {
                    if let Some(panel) = panel.upgrade() {
                        panel
                            .update(cx, |panel, cx| {
                                panel.error = Some(format!("{}", err).into());
                                panel.is_loading = false;
                                cx.notify();
                            })
                            .ok();
                    }
                }
            }));
        } else {
            self.error = Some("No JJ repositories detected".into());
            self.is_loading = false;
            cx.notify();
        }
    }

    fn focus_in(this: &mut Self, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(PanelEvent::Activate);
    }

    fn format_timestamp(timestamp: i64) -> String {
        let nanos = (timestamp as i128) * 1_000_000;
        OffsetDateTime::from_unix_timestamp_nanos(nanos)
            .ok()
            .and_then(|time| time.format(&Rfc3339).ok())
            .unwrap_or_else(|| "unknown time".to_string())
    }

    fn short_hex(id: &SharedString) -> String {
        id.chars().take(12).collect()
    }

    fn refresh_action(&mut self, _: &Button, window: &mut Window, cx: &mut Context<Self>) {
        self.request_refresh(window, cx);
    }

    fn render_commits(&self) -> impl IntoElement + '_ {
        v_flex()
            .gap(rems(0.25))
            .children(self.commits.iter().map(|commit| {
                let timestamp = Self::format_timestamp(commit.timestamp);
                v_flex()
                    .gap(rems(0.1))
                    .child(
                        h_flex()
                            .justify_between()
                            .child(Label::new(commit.description.clone()).size(LabelSize::Medium))
                            .child(
                                Label::new(timestamp)
                                    .color(Color::Muted)
                                    .size(LabelSize::XSmall),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap(rems(0.5))
                            .child(
                                Label::new(format!(
                                    "commit {}",
                                    Self::short_hex(&commit.commit_id)
                                ))
                                .size(LabelSize::XSmall)
                                .color(Color::Muted),
                            )
                            .child(
                                Label::new(format!(
                                    "change {}",
                                    Self::short_hex(&commit.change_id)
                                ))
                                .size(LabelSize::XSmall)
                                .color(Color::Muted),
                            )
                            .child(
                                Label::new(commit.author.clone())
                                    .size(LabelSize::XSmall)
                                    .color(Color::Placeholder),
                            ),
                    )
            }))
    }
}

impl Focusable for JjPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<PanelEvent> for JjPanel {}

impl Panel for JjPanel {
    fn persistent_name() -> &'static str {
        "JjPanel"
    }

    fn panel_key() -> &'static str {
        "JjPanel"
    }

    fn position(&self, _: &Window, _: &App) -> DockPosition {
        DockPosition::Left
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(position, DockPosition::Left | DockPosition::Right)
    }

    fn set_position(&mut self, _: DockPosition, _: &mut Window, _: &mut Context<Self>) {}

    fn size(&self, _: &Window, _: &App) -> Pixels {
        px(320.0)
    }

    fn set_size(&mut self, _: Option<Pixels>, _: &mut Window, _: &mut Context<Self>) {}

    fn icon(&self, _: &Window, _: &App) -> Option<IconName> {
        Some(IconName::GitBranch)
    }

    fn icon_tooltip(&self, _: &Window, _: &App) -> Option<&'static str> {
        Some("Jujutsu Panel")
    }

    fn toggle_action(&self) -> Box<dyn Action> {
        Box::new(ToggleFocus)
    }

    fn activation_priority(&self) -> u32 {
        3
    }

    fn enabled(&self, cx: &App) -> bool {
        cx.has_flag::<JjUiFeatureFlag>()
    }
}

impl Render for JjPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = h_flex()
            .justify_between()
            .items_center()
            .padding(px(4.0))
            .child(Label::new("JJ History").size(LabelSize::Large))
            .child(
                Button::new("refresh-jj", "Refresh")
                    .style(ButtonStyle::Secondary)
                    .on_click(cx.listener(Self::refresh_action)),
            );

        let content: AnyElement = if self.is_loading {
            Label::new("Loading commits…").into()
        } else if let Some(error) = &self.error {
            Label::new(error.clone()).color(Color::Error).into()
        } else if self.commits.is_empty() {
            Label::new("No commits to show").color(Color::Muted).into()
        } else {
            Scroll::new(self.render_commits()).into()
        };

        v_flex()
            .gap(rems(0.5))
            .padding(rems(0.5))
            .child(header)
            .child(content)
    }
}

pub type JjPanelLoadError = anyhow::Error;
