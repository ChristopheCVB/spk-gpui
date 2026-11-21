use gpui_kit::component::Disableable;

use super::*;
use crate::api::ChangelogEntry;

pub(super) struct History {
    commits: Vec<ChangelogEntry>,
    head: Option<String>,
    page: Option<u32>,
    loading: bool,
    failed: bool,
}

impl Default for History {
    fn default() -> Self {
        Self {
            commits: Vec::new(),
            head: None,
            page: Some(1),
            loading: false,
            failed: false,
        }
    }
}

impl History {
    pub(super) fn initial(&self) -> bool {
        self.page == Some(1) && !self.failed
    }
}

impl Settings {
    pub(super) fn load_changelog(&mut self, cx: &mut Context<Self>) {
        let Some(page) = self.changelog.page.filter(|_| !self.changelog.loading) else {
            return;
        };
        let (reply, result) = tokio::sync::oneshot::channel();
        let head = self.changelog.head.clone();
        self.changelog.loading = true;
        self.changelog.failed = false;
        self.spk.update(cx, |app, cx| {
            app.send(Command::LoadChangelog { page, head, reply }, None, cx)
        });
        cx.spawn(async move |settings, cx| {
            let result = result.await;
            let _ = settings.update(cx, |settings, cx| {
                let history = &mut settings.changelog;
                history.loading = false;
                if let Ok(Ok(page)) = result {
                    let mut seen: std::collections::HashSet<_> = history
                        .commits
                        .iter()
                        .map(|entry| entry.sha.clone())
                        .collect();
                    history.commits.extend(
                        page.commits
                            .into_iter()
                            .filter(|entry| seen.insert(entry.sha.clone())),
                    );
                    history.commits.sort_by(|a, b| b.date.cmp(&a.date));
                    history.head = page.head;
                    history.page = page.next_page;
                } else {
                    history.failed = true;
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn changelog(&self, cx: &Context<Self>) -> impl IntoElement {
        let history = &self.changelog;
        let theme = cx.theme();
        let mut day = "";
        div()
            .flex()
            .flex_col()
            .items_start()
            .gap_2()
            .child(Label::new("Changelog").text_base().font_semibold())
            .children(history.commits.iter().map(|entry| {
                let new_day = day != entry.date;
                day = &entry.date;
                let url = format!("https://github.com/meeehdi-dev/spk/commit/{}", entry.sha);
                div()
                    .when(new_day, |group| {
                        group.child(
                            Label::new(entry.date.clone())
                                .text_color(theme.muted_foreground)
                                .font_semibold()
                                .pt_3(),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                Button::new(SharedString::from(entry.sha.clone()))
                                    .ghost()
                                    .small()
                                    .tab_stop(true)
                                    .label(entry.sha[..7].to_owned())
                                    .accessibility_label(format!("Open commit {}", &entry.sha[..7]))
                                    .on_click(move |_, _, cx| cx.open_url(&url)),
                            )
                            .child(Label::new(entry.message.clone()).min_w_0()),
                    )
            }))
            .when(history.failed, |list| {
                list.child(
                    Label::new("Changelog is unavailable. Try again later.")
                        .text_color(theme.danger),
                )
            })
            .when(history.page.is_some(), |list| {
                list.child(
                    Button::new("changelog-more")
                        .small()
                        .tab_stop(true)
                        .disabled(history.loading)
                        .label(if history.loading {
                            "Loading commits…"
                        } else if history.failed {
                            "Retry"
                        } else {
                            "Load more"
                        })
                        .on_click(cx.listener(|this, _, _, cx| this.load_changelog(cx))),
                )
            })
            .when(history.page.is_none(), |list| {
                list.child(
                    Label::new(if history.commits.is_empty() {
                        "No commits yet."
                    } else {
                        "End of commit history."
                    })
                    .text_color(theme.muted_foreground),
                )
            })
    }
}
