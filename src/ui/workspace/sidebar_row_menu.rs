use gpui_kit::base::actions::Cancel;

use super::*;

#[derive(Clone)]
struct MenuState {
    focus: FocusHandle,
    hovered: Rc<Cell<bool>>,
    open: Rc<Cell<bool>>,
}

pub(super) struct SidebarRowMenu {
    state: MenuState,
    visible: bool,
}

impl SidebarRowMenu {
    pub(super) fn attach(
        row: &mut Stateful<Div>,
        focused: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> Self {
        let row = row.interactivity();
        let id = row.element_id.clone().expect("sidebar row has an ID");
        let state = window.with_element_namespace(id, |window| {
            window
                .use_keyed_state("sidebar-row-menu", cx, |_, cx| MenuState {
                    focus: cx.focus_handle(),
                    hovered: Rc::default(),
                    open: Rc::default(),
                })
                .read(cx)
                .clone()
        });
        let hovered = state.hovered.clone();
        row.hover_listener_mode(HoverListenerMode::InputModalityIndependent);
        row.on_hover(move |hover, window, _| {
            hovered.set(*hover);
            window.refresh();
        });
        let visible =
            state.hovered.get() || focused || state.focus.is_focused(window) || state.open.get();
        Self { state, visible }
    }

    pub(super) fn focus(&self) -> FocusHandle {
        self.state.focus.clone()
    }

    pub(super) fn render(
        self,
        button: Button,
        enabled: bool,
        anchor: Anchor,
        items: impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static,
    ) -> AnyElement {
        if !self.visible {
            // Keep the same tab stop without reserving space or creating a button hit target.
            return div()
                .absolute()
                .size_0()
                .track_focus(&self.state.focus.tab_stop(enabled))
                .into_any_element();
        }
        let open = self.state.open;
        let focus = self.state.focus;
        div()
            .flex_shrink_0()
            .capture_action({
                let open = open.clone();
                let focus = focus.clone();
                move |_: &Cancel, window, cx| {
                    // Move focus before the popup handles Escape and restores its saved focus.
                    if open.get() {
                        focus.focus(window, cx);
                    }
                }
            })
            .child(
                button
                    .disabled(!enabled)
                    .track_focus(&focus)
                    .dropdown_menu_with_anchor(anchor, items)
                    .on_open_change(move |is_open, window, _| {
                        open.set(*is_open);
                        window.refresh();
                    }),
            )
            .into_any_element()
    }
}
