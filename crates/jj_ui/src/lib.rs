use anyhow::{Context as _, Result, anyhow};
use command_palette_hooks::CommandPaletteFilter;
use editor::Editor;
use feature_flags::{FeatureFlagAppExt as _, JjUiFeatureFlag};
use gpui::{
    Action, App, AsyncWindowContext, ClickEvent, Context, Corner, DismissEvent, Entity,
    EventEmitter, FocusHandle, Focusable, MouseButton, MouseDownEvent, Pixels, Point, SharedString,
    Subscription, Task, WeakEntity, Window, actions, px, rems,
};
use jj::{short_change_hash, short_commit_hash};
use log::{info, warn};
use project::{JjCommitSummary, JjRepositorySummary, Project, ProjectEntryId};
use std::time::Duration;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use ui::{
    AnyElement, ButtonStyle, ContextMenu, Modal, ModalFooter, ModalHeader, Section, prelude::*,
};
use ui_input::InputField;
use workspace::{
    ModalView, Workspace,
    dock::{DockPosition, Panel, PanelEvent},
};

actions!(jj_ui, [ToggleFocus, OpenDiff]);

#[derive(Clone)]
struct CommitMenuTarget {
    repo_id: ProjectEntryId,
    commit: JjCommitSummary,
}

pub fn init(cx: &mut App) {
    info!(target: "jj_ui", "starting to init.");
    if !cx.has_flag::<JjUiFeatureFlag>() {
        return;
    }
    CommandPaletteFilter::update_global(cx, |filter, _| {
        filter.show_namespace("jj_ui");
    });
    info!(target: "jj_ui", "jj_ui inited.");

    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace.register_action(|workspace, _: &ToggleFocus, window, cx| {
            info!(target: "jj_ui", "ToggleFocus action invoked");
            let opened = workspace.toggle_panel_focus::<JjPanel>(window, cx);
            info!(target: "jj_ui", "toggle_panel_focus result: opened={}", opened);
        });
        workspace.register_action(|workspace, _: &OpenDiff, window, cx| {
            info!(target: "jj_ui", "OpenDiff action invoked");
            if let Err(err) = open_unstaged_diff_for_active_editor(workspace, window, cx) {
                info!(target: "jj_ui", "OpenDiff failed: {err:?}");
            }
        });
    })
    .detach();
}

pub struct JjPanel {
    _workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    focus_handle: FocusHandle,
    commits: Vec<JjCommitSummary>,
    is_loading: bool,
    error: Option<SharedString>,
    _task: Option<Task<()>>,
    repositories: Vec<JjRepositorySummary>,
    selected_repo: Option<ProjectEntryId>,
    _store_subscription: Option<Subscription>,
    context_menu: Option<(Entity<ContextMenu>, Point<Pixels>, Subscription)>,
}

impl JjPanel {
    pub fn new(
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<Self> {
        let project = workspace.project().clone();
        let panel_workspace = workspace.weak_handle();
        cx.new(|cx| {
            let focus_handle = cx.focus_handle();
            cx.on_focus(&focus_handle, window, Self::focus_in).detach();
            let mut panel = Self {
                _workspace: panel_workspace,
                project,
                focus_handle,
                commits: Vec::new(),
                is_loading: true,
                error: None,
                _task: None,
                repositories: Vec::new(),
                selected_repo: None,
                _store_subscription: None,
                context_menu: None,
            };
            panel.request_refresh(window, cx);
            panel.ensure_store_subscription(window, cx);
            panel
        })
    }

    pub async fn load(
        workspace: WeakEntity<Workspace>,
        mut cx: AsyncWindowContext,
    ) -> Result<Entity<Self>> {
        workspace.update_in(&mut cx, |workspace, window, cx| {
            let panel = Self::new(workspace, window, cx);
            info!(target: "jj_ui", "JJ panel entity created");
            Ok(panel)
        })?
    }

    fn request_refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let jj_store = self.project.read(cx).jj_store().cloned();
        self.ensure_store_subscription(window, cx);
        let mut updated = false;
        match &jj_store {
            Some(store) => {
                let repos = store.read(cx).repositories();
                if repos != self.repositories {
                    self.repositories = repos.clone();
                    updated = true;
                }
                if let Some(selected) = self.selected_repo {
                    if !self.repositories.iter().any(|repo| repo.id == selected) {
                        self.selected_repo = self.repositories.first().map(|repo| repo.id);
                        updated = true;
                    }
                } else if !self.repositories.is_empty() {
                    self.selected_repo = self.repositories.first().map(|repo| repo.id);
                    updated = true;
                }
            }
            None => {
                if !self.repositories.is_empty() || self.selected_repo.is_some() {
                    self.repositories.clear();
                    self.selected_repo = None;
                    updated = true;
                }
            }
        }
        if updated {
            cx.notify();
        }

        let Some(jj_store) = jj_store else {
            self.error = Some("JJ support unavailable".into());
            self.is_loading = false;
            cx.notify();
            return;
        };
        if self.repositories.is_empty() {
            self.error = Some("No JJ repositories detected".into());
            self.is_loading = false;
            cx.notify();
            return;
        }
        self.is_loading = true;
        self.error = None;
        cx.notify();
        let selected_repo = self.selected_repo;
        if let Some(task) =
            jj_store.update(cx, |store, cx| store.recent_commits(selected_repo, 50, cx))
        {
            let panel = cx.weak_entity();
            self._task = Some(cx.spawn_in(window, async move |_, cx| match task.await {
                Ok(commits) => {
                    if let Some(panel) = panel.upgrade() {
                        let _ = panel.update(cx, |panel, cx| {
                            panel.commits = commits;
                            panel.is_loading = false;
                            panel.error = None;
                            cx.notify();
                        });
                    }
                }
                Err(err) => {
                    if let Some(panel) = panel.upgrade() {
                        let _ = panel.update(cx, |panel, cx| {
                            panel.error = Some(format!("{err}").into());
                            panel.is_loading = false;
                            cx.notify();
                        });
                    }
                }
            }));
        } else {
            self.error = Some("No JJ repositories detected".into());
            self.is_loading = false;
            cx.notify();
        }
    }

    fn ensure_store_subscription(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(store) = self.project.read(cx).jj_store().cloned() {
            if self._store_subscription.is_none() {
                let subscription = cx.observe_in(&store, window, |panel, _, window, cx| {
                    panel.handle_store_updated(window, cx);
                });
                self._store_subscription = Some(subscription);
            }
        } else {
            self._store_subscription.take();
        }
    }

    fn handle_store_updated(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.request_refresh(window, cx);
    }

    fn focus_in(_this: &mut Self, _: &mut Window, cx: &mut Context<Self>) {
        info!(target: "jj_ui", "JJ panel focused");
        cx.emit(PanelEvent::Activate);
    }

    fn format_timestamp(timestamp: i64) -> String {
        let nanos = (timestamp as i128) * 1_000_000;
        OffsetDateTime::from_unix_timestamp_nanos(nanos)
            .ok()
            .and_then(|time| time.format(&Rfc3339).ok())
            .unwrap_or_else(|| "unknown time".to_string())
    }

    fn refresh_action(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        info!(
            target: "jj_ui",
            "refresh pressed (selected_repo={:?})",
            self.selected_repo
        );
        self.request_refresh(window, cx);
    }

    fn select_repository(
        &mut self,
        repo_id: ProjectEntryId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected_repo == Some(repo_id) {
            return;
        }
        self.selected_repo = Some(repo_id);
        self.request_refresh(window, cx);
    }

    fn close_context_menu(&mut self, cx: &mut Context<Self>) {
        if self.context_menu.is_some() {
            self.context_menu.take();
            cx.notify();
        }
    }

    fn trigger_edit_change(
        &mut self,
        commit: &JjCommitSummary,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        let Some(repo_id) = self.selected_repo else {
            return;
        };
        let Some(store) = self.project.read(cx).jj_store().cloned() else {
            self.error = Some("JJ support unavailable".into());
            cx.notify();
            return;
        };
        let change_id = commit.change_id.clone();
        if let Some(task) = store.update(cx, |store, cx| store.edit_change(repo_id, change_id, cx))
        {
            self.spawn_store_task("jj edit", task, window, cx);
        }
    }

    fn show_rename_modal(
        &mut self,
        target: CommitMenuTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        let Some(workspace) = self._workspace.upgrade() else {
            return;
        };
        let project = self.project.clone();
        let _ = workspace.update(cx, |workspace, cx| {
            workspace.toggle_modal(window, cx, move |window, cx| {
                RenameChangeModal::new(project.clone(), target.clone(), window, cx)
            });
        });
    }

    fn deploy_commit_context_menu(
        &mut self,
        target: CommitMenuTarget,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panel = cx.entity().downgrade();
        let menu = ContextMenu::build(window, cx, move |menu, _window, _cx| {
            let rename_target = target.clone();
            let rename_panel = panel.clone();
            menu.entry("Rename change…", None, move |window, cx| {
                if let Some(panel) = rename_panel.upgrade() {
                    let _ = panel.update(cx, |panel, cx| {
                        panel.show_rename_modal(rename_target.clone(), window, cx);
                    });
                }
            })
        });
        self.set_context_menu(menu, position, window, cx);
    }

    fn set_context_menu(
        &mut self,
        menu: Entity<ContextMenu>,
        position: Point<Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let subscription =
            cx.subscribe_in(&menu, window, |this, _, _: &DismissEvent, window, cx| {
                if this.context_menu.as_ref().is_some_and(|(open_menu, _, _)| {
                    open_menu.focus_handle(cx).contains_focused(window, cx)
                }) {
                    window.focus(&this.focus_handle);
                }
                this.context_menu.take();
                cx.notify();
            });
        self.context_menu = Some((menu, position, subscription));
        cx.notify();
    }

    fn spawn_store_task(
        &self,
        label: &'static str,
        task: Task<Result<()>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panel = cx.entity().downgrade();
        cx.spawn_in(window, async move |_, cx| match task.await {
            Ok(_) => info!(target: "jj_ui", "{label} completed"),
            Err(err) => {
                warn!(target: "jj_ui", "{label} failed: {err:?}");
                if let Some(panel) = panel.upgrade() {
                    panel
                        .update(cx, |panel, cx| {
                            panel.error = Some(format!("{err}").into());
                            cx.notify();
                        })
                        .ok();
                }
            }
        })
        .detach();
    }

    fn render_repository_selector(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.repositories.len() <= 1 {
            return None;
        }

        let repos = self.repositories.clone();

        Some(
            h_flex()
                .gap(rems(0.25))
                .children(repos.into_iter().map(|repo| {
                    let is_selected = self.selected_repo == Some(repo.id);
                    let label = repo.path.clone();
                    Button::new(("jj-repo", repo.id.to_proto()), label)
                        .style(if is_selected {
                            ButtonStyle::Filled
                        } else {
                            ButtonStyle::Outlined
                        })
                        .on_click(cx.listener(move |panel, _, window, cx| {
                            panel.select_repository(repo.id, window, cx);
                        }))
                }))
                .into_any(),
        )
    }

    fn current_repository_label(&self) -> Option<SharedString> {
        let selected = self.selected_repo?;
        self.repositories
            .iter()
            .find(|repo| repo.id == selected)
            .map(|repo| repo.path.clone())
    }

    fn render_commits(&mut self, cx: &mut Context<Self>) -> impl IntoElement + '_ {
        v_flex()
            .gap(rems(0.25))
            .children(self.commits.iter().cloned().map(|commit| {
                let timestamp = Self::format_timestamp(commit.timestamp);
                let change_short = short_change_hash(&commit.change_id);
                let commit_short = short_commit_hash(&commit.commit_id);
                let description = commit.description.clone();
                let author = commit.author.clone();
                let click_commit = commit.clone();
                let menu_commit = commit.clone();

                let mut title_row = h_flex().gap(rems(0.25)).items_center();
                if commit.is_current {
                    title_row = title_row
                        .child(Label::new("•").color(Color::Accent).size(LabelSize::Small));
                }
                title_row = title_row.child(Label::new(description).size(LabelSize::Default));

                let body = v_flex()
                    .gap(rems(0.1))
                    .child(
                        h_flex().justify_between().child(title_row).child(
                            Label::new(timestamp)
                                .color(Color::Muted)
                                .size(LabelSize::XSmall),
                        ),
                    )
                    .child(
                        h_flex()
                            .gap(rems(0.5))
                            .child(
                                Label::new(format!("commit {commit_short}"))
                                    .size(LabelSize::XSmall)
                                    .color(Color::Muted),
                            )
                            .child(
                                Label::new(format!("change {change_short}"))
                                    .size(LabelSize::XSmall)
                                    .color(Color::Muted),
                            )
                            .child(
                                Label::new(author)
                                    .size(LabelSize::XSmall)
                                    .color(Color::Placeholder),
                            ),
                    );

                let interactive = self.selected_repo.is_some();
                let mut wrapper = div().rounded(px(4.0)).p(px(4.0)).child(body);

                if commit.is_current {
                    wrapper = wrapper
                        .border_1()
                        .border_color(cx.theme().colors().border_focused)
                        .bg(cx.theme().colors().surface_background);
                }

                if interactive {
                    wrapper = wrapper
                        .cursor_pointer()
                        .hover(|el| el.bg(cx.theme().colors().surface_background))
                        .on_mouse_down(MouseButton::Left, |_, window, _| {
                            window.prevent_default();
                        })
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(move |panel, _, window, cx| {
                                panel.trigger_edit_change(&click_commit, window, cx);
                            }),
                        );
                } else {
                    wrapper = wrapper.opacity(0.75);
                }

                if interactive {
                    wrapper = wrapper.on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |panel, event: &MouseDownEvent, window, cx| {
                            window.prevent_default();
                            let Some(repo_id) = panel.selected_repo else {
                                return;
                            };
                            panel.deploy_commit_context_menu(
                                CommitMenuTarget {
                                    repo_id,
                                    commit: menu_commit.clone(),
                                },
                                event.position,
                                window,
                                cx,
                            );
                        }),
                    );
                }

                wrapper
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

    fn icon(&self, _: &Window, _: &App) -> Option<ui::IconName> {
        Some(ui::IconName::GitBranch)
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = h_flex()
            .justify_between()
            .items_center()
            .p(px(4.0))
            .child(Label::new("JJ History").size(LabelSize::Large))
            .child(
                Button::new("refresh-jj", "Refresh")
                    .style(ButtonStyle::Outlined)
                    .on_click(cx.listener(Self::refresh_action)),
            );

        let repo_selector = self.render_repository_selector(window, cx);
        let repo_label = self.current_repository_label();

        let content: AnyElement = if self.is_loading {
            Label::new("Loading commits…").into_any_element()
        } else if let Some(error) = &self.error {
            Label::new(error.clone())
                .color(Color::Error)
                .into_any_element()
        } else if self.commits.is_empty() {
            Label::new("No commits to show")
                .color(Color::Muted)
                .into_any_element()
        } else {
            div().child(self.render_commits(cx)).into_any()
        };

        let mut layout = v_flex().gap(rems(0.5)).p(rems(0.5)).child(header);

        if let Some(label) = repo_label {
            layout = layout.child(Label::new(label).size(LabelSize::Small).color(Color::Muted));
        }

        if let Some(selector) = repo_selector {
            layout = layout.child(selector);
        }

        layout = layout.child(content);

        if let Some((menu, position, _)) = &self.context_menu {
            layout = layout.child(
                deferred(
                    anchored()
                        .position(*position)
                        .anchor(Corner::TopLeft)
                        .child(menu.clone()),
                )
                .with_priority(1),
            );
        }

        layout
    }
}

fn open_unstaged_diff_for_active_editor(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Result<()> {
    let Some(editor) = workspace.active_item_as::<Editor>(cx) else {
        return Err(anyhow!("no active editor"));
    };
    let buffer_entity = {
        let editor = editor.read(cx);
        let multi = editor.buffer().read(cx);
        multi
            .as_singleton()
            .context("active editor has no single buffer")?
    };
    let project = workspace.project().clone();
    let buffer_for_log = buffer_entity.clone();
    let task = project.update(cx, |project, cx| {
        project.open_unstaged_diff(buffer_entity.clone(), cx)
    });
    cx.spawn_in(window, async move |_, cx| match task.await {
        Ok(diff_entity) => {
            info!(target: "jj_ui", "open_unstaged_diff completed; collecting diff details");
            match cx.update(|_, app| {
                let working_snapshot = buffer_for_log.read(app).snapshot();
                let working_text = working_snapshot.text.text();
                let diff_read = diff_entity.read(app);
                let base_text = diff_read.base_text().text.text();
                let hunks: Vec<_> = diff_read.hunks(&working_snapshot.text, app).collect();
                (working_text, base_text, hunks)
            }) {
                Ok((working_text, base_text, hunks)) => {
                    info!(
                        target: "jj_ui",
                        "open_unstaged_diff base_preview={} working_preview={}",
                        summarize_text_for_log(&base_text),
                        summarize_text_for_log(&working_text)
                    );
                    if hunks.is_empty() {
                        info!(target: "jj_ui", "open_unstaged_diff hunks: none");
                    } else {
                        info!(
                            target: "jj_ui",
                            "open_unstaged_diff hunks total={}",
                            hunks.len()
                        );
                        for (index, hunk) in hunks.iter().enumerate() {
                            info!(target: "jj_ui", "open_unstaged_diff hunk {index}: {hunk:?}");
                        }
                    }
                }
                Err(err) => {
                    info!(
                        target: "jj_ui",
                        "open_unstaged_diff succeeded but logging failed: {err:?}"
                    );
                }
            }
        }
        Err(err) => info!(target: "jj_ui", "open_unstaged_diff failed: {err:?}"),
    })
    .detach();
    Ok(())
}

fn summarize_text_for_log(text: &str) -> String {
    const MAX_PREVIEW_CHARS: usize = 120;
    if text.is_empty() {
        return "<empty>".into();
    }
    let single_line = text.replace('\n', "\\n");
    if single_line.len() > MAX_PREVIEW_CHARS {
        format!(
            "{}… (len={})",
            &single_line[..MAX_PREVIEW_CHARS],
            single_line.len()
        )
    } else {
        format!("{single_line} (len={})", single_line.len())
    }
}

struct RenameChangeModal {
    focus_handle: FocusHandle,
    input: Entity<InputField>,
    project: Entity<Project>,
    target: CommitMenuTarget,
    is_submitting: bool,
    error: Option<SharedString>,
}

impl RenameChangeModal {
    fn new(
        project: Entity<Project>,
        target: CommitMenuTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            InputField::new(window, cx, "New change description")
                .label("Description")
                .label_size(LabelSize::Small)
        });
        input.update(cx, |field, cx| {
            field.set_text(target.commit.description.clone(), window, cx);
        });
        input.update(cx, |field, cx| {
            let editor = field.editor().clone();
            editor.update(cx, |editor, cx| {
                let focus = editor.focus_handle(cx);
                window.focus(&focus);
            });
        });
        Self {
            focus_handle: cx.focus_handle(),
            input,
            project,
            target,
            is_submitting: false,
            error: None,
        }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_submitting {
            return;
        }
        let description = self.input.read(cx).text(cx).trim().to_string();
        if description.is_empty() {
            self.error = Some("Description cannot be empty".into());
            cx.notify();
            return;
        }
        let Some(store) = self.project.read(cx).jj_store().cloned() else {
            self.error = Some("JJ support unavailable".into());
            cx.notify();
            return;
        };
        let change_id = self.target.commit.change_id.clone();
        let repo_id = self.target.repo_id;
        if let Some(task) = store.update(cx, |store, cx| {
            store.rename_change(repo_id, change_id.clone(), description.clone(), cx)
        }) {
            self.is_submitting = true;
            let modal = cx.entity().downgrade();
            cx.spawn_in(window, async move |_, cx| match task.await {
                Ok(_) => {
                    if let Some(modal) = modal.upgrade() {
                        let _ = modal.update(cx, |_, cx| cx.emit(DismissEvent));
                    }
                }
                Err(err) => {
                    warn!(target: "jj_ui", "rename change failed: {err:?}");
                    if let Some(modal) = modal.upgrade() {
                        let _ = modal.update(cx, |modal, cx| {
                            modal.is_submitting = false;
                            modal.error = Some(format!("{err}").into());
                            cx.notify();
                        });
                    }
                }
            })
            .detach();
        }
    }
}

impl ModalView for RenameChangeModal {}

impl EventEmitter<DismissEvent> for RenameChangeModal {}

impl Focusable for RenameChangeModal {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RenameChangeModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let change_short = short_change_hash(&self.target.commit.change_id);
        let header = ModalHeader::new().headline(format!("Rename change {change_short}"));

        let mut body = v_flex().gap(rems(0.5)).child(self.input.clone());

        if let Some(error) = &self.error {
            body = body.child(Label::new(error.clone()).color(Color::Error));
        }

        let footer_actions = h_flex()
            .gap(rems(0.5))
            .child(
                Button::new("rename-cancel", "Cancel")
                    .style(ButtonStyle::Transparent)
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.emit(DismissEvent);
                    })),
            )
            .child(
                Button::new("rename-submit", "Rename")
                    .style(ButtonStyle::Filled)
                    .disabled(self.is_submitting)
                    .on_click(cx.listener(|modal, _, window, cx| {
                        modal.submit(window, cx);
                    })),
            );

        let footer = ModalFooter::new().end_slot(footer_actions);

        let section = Section::new().child(body);

        Modal::new("rename-change", None)
            .header(header)
            .section(section)
            .footer(footer)
    }
}
