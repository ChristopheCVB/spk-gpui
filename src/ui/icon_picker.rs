use gpui_kit::component::{IndexPath, select::SearchableVec};

use super::*;

pub(super) type IconPicker = Entity<SelectState<SearchableVec<IconOption>>>;

#[derive(Clone)]
pub(super) struct IconOption {
    name: SharedString,
    icon: Icon,
}

impl IconOption {
    fn content(&self) -> impl IntoElement {
        control_content()
            .child(self.icon.clone().size(px(16.)))
            .child(div().min_w_0().truncate().child(self.name.clone()))
    }
}

impl SelectItem for IconOption {
    type Value = SharedString;

    fn title(&self) -> SharedString {
        self.name.clone()
    }

    fn value(&self) -> &Self::Value {
        &self.name
    }

    fn render(&self, _: &mut Window, _: &mut App) -> impl IntoElement {
        self.content()
    }

    fn display_title(&self) -> Option<AnyElement> {
        Some(self.content().into_any_element())
    }

    fn matches(&self, query: &str) -> bool {
        let query = query.split_whitespace().collect::<Vec<_>>().join("-");
        self.name.contains(&query.to_ascii_lowercase())
    }
}

fn icon_options(selected: &str, fallback: impl FnOnce(&str) -> Icon) -> (Vec<IconOption>, usize) {
    let mut icons: Vec<_> = assets::AllAssets
        .list("icons/")
        .unwrap_or_default()
        .into_iter()
        .filter_map(|path| {
            let name = path.strip_prefix("icons/")?.strip_suffix(".svg")?;
            crate::api::has_icon_name_characters(name).then(|| IconOption {
                name: name.to_owned().into(),
                icon: Icon::default().path(path.clone()),
            })
        })
        .collect();
    icons.sort_unstable_by(|left, right| left.name.cmp(&right.name));
    let index = match icons.binary_search_by(|icon| icon.name.as_ref().cmp(selected)) {
        Ok(index) => index,
        Err(index) => {
            icons.insert(
                index,
                IconOption {
                    name: selected.to_owned().into(),
                    icon: fallback(selected),
                },
            );
            index
        }
    };
    (icons, index)
}

pub(super) fn icon_picker(
    selected: &str,
    fallback: impl FnOnce(&str) -> Icon,
    window: &mut Window,
    cx: &mut App,
) -> IconPicker {
    let (icons, index) = icon_options(selected, fallback);
    cx.new(|cx| {
        SelectState::new(
            SearchableVec::new(icons),
            Some(IndexPath::default().row(index)),
            window,
            cx,
        )
        .searchable(true)
    })
}

pub(super) fn icon_picker_field(picker: &IconPicker, disabled: bool) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .flex_1()
        .min_w_0()
        .child(Label::new("Icon"))
        .child(
            control_select(picker)
                .w_full()
                .accessibility_label("Icon")
                .placeholder("Choose icon")
                .search_placeholder("Search icons")
                .disabled(disabled),
        )
}

#[cfg(test)]
mod tests {
    use super::{Icon, IconName, SelectItem, icon_options};

    #[test]
    fn icon_search_matches_partial_names_with_spaces_and_mixed_case() {
        let (icons, index) = icon_options("audio-lines", |_| panic!("bundled icon"));
        let icon = &icons[index];
        assert_eq!(icon.value().as_ref(), "audio-lines");
        assert!(icon.matches("Audio  Lin"));
        assert!(icon.matches("DIO-LI"));
        assert!(!icon.matches("audio wave"));
    }

    #[test]
    fn unbundled_icon_uses_fallback_and_retains_its_saved_name() {
        let saved_name = "spk-unbundled-test-icon";
        let (icons, index) = icon_options(saved_name, |name| {
            assert_eq!(name, saved_name);
            Icon::new(IconName::Megaphone)
        });
        assert_eq!(icons[index].title().as_ref(), saved_name);
        assert_eq!(icons[index].value().as_ref(), saved_name);
    }
}
