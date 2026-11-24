use gpui_kit::component::{IndexPath, select::SearchableVec};

use super::*;

pub(super) type IconPicker = Entity<SelectState<SearchableVec<SharedString>>>;

pub(super) fn bundled_icon_names() -> Vec<SharedString> {
    let mut icons: Vec<SharedString> = assets::AllAssets
        .list("icons/")
        .unwrap_or_default()
        .iter()
        .filter_map(|path| path.strip_prefix("icons/")?.strip_suffix(".svg"))
        .filter(|name| {
            name.bytes()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == b'-')
        })
        .map(|name| name.to_owned().into())
        .collect();
    icons.sort_unstable();
    icons
}

pub(super) fn icon_picker(
    icons: &[SharedString],
    selected: &str,
    window: &mut Window,
    cx: &mut App,
) -> IconPicker {
    let selected = icons.iter().position(|icon| icon.as_ref() == selected);
    cx.new(|cx| {
        SelectState::new(
            SearchableVec::new(icons.to_vec()),
            selected.map(|index| IndexPath::default().row(index)),
            window,
            cx,
        )
        .searchable(true)
    })
}

pub(super) fn icon_picker_field(
    picker: &IconPicker,
    original: Option<&str>,
    icon: impl FnOnce(&str) -> Icon,
    disabled: bool,
    cx: &App,
) -> impl IntoElement {
    let selected = picker
        .read(cx)
        .selected_value()
        .map(AsRef::as_ref)
        .or(original);
    div()
        .flex()
        .flex_col()
        .gap_1()
        .flex_1()
        .min_w_0()
        .child(Label::new("Icon"))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .children(selected.map(icon))
                .child(
                    control_select(picker)
                        .w_full()
                        .accessibility_label("Icon")
                        .placeholder(original.unwrap_or("Choose icon").to_owned())
                        .search_placeholder("Search icons")
                        .disabled(disabled),
                ),
        )
}
