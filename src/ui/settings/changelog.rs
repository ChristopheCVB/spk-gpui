use gpui_kit::base::text::{MarkdownExtensions, TextView, TextViewStyle};

use super::*;

const RELEASE_NOTES: &str = include_str!("../../../CHANGELOG.md");

impl Settings {
    pub(super) fn changelog(&self, cx: &Context<Self>) -> impl IntoElement {
        div()
            .id("desktop-release-notes")
            .role(Role::Group)
            .aria_label("Desktop release notes")
            .child(
                TextView::markdown("desktop-changelog", RELEASE_NOTES)
                    .style(
                        TextViewStyle::from_theme(&gpui_kit::base::Theme::global(cx))
                            .with_link(cx.theme().primary),
                    )
                    .markdown_extensions(MarkdownExtensions::default().raw_html(false))
                    .on_link_click(|href, _, _, cx| {
                        if href.starts_with("https://") {
                            cx.open_url(href);
                        }
                    }),
            )
    }
}
