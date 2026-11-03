//! The Settings view. Today it holds Appearance: theme, window, text and conversation choices,
//! each applied the moment it is clicked.

use std::rc::Rc;

use gpui::{
    Anchor, AnyElement, App, FontWeight, Hsla, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement, SharedString, StatefulInteractiveElement as _, Styled, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_kit::base::Selectable as _;
use gpui_kit::base::TypographyTokens;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _};
use newjen_core::appearance::{
    Accent, Appearance, ChatWidth, CodeSize, DiffColors, Font, Glass, Mode, TextSize,
};

use crate::theme::{self, material};
use crate::ui::IconName;

/// Sets the whole look, as changed by one click. The owner of the view applies and saves it.
pub type OnChange = Rc<dyn Fn(Appearance, &mut Window, &mut App)>;

/// What the view draws.
pub struct Input<'a> {
    pub appearance: &'a Appearance,
    pub on_change: OnChange,
}

const COLUMN: f32 = 600.0;
const MENU_WIDTH: f32 = 190.0;
const SWATCH: f32 = 16.0;
const SYSTEM_SANS: &str = "System (SF Pro)";
const INTERFACE_FONTS: &[&str] = &[
    "Helvetica Neue",
    "Avenir Next",
    "Inter",
    "Menlo",
    "SF Mono",
    "Monaco",
    "JetBrains Mono",
];
const CODE_FONTS: &[&str] = &[
    "Menlo",
    "SF Mono",
    "Monaco",
    "JetBrains Mono",
    "Fira Code",
    "Courier New",
];

pub fn render(input: &Input, cx: &App) -> AnyElement {
    let look = input.appearance;
    let installed = theme::installed_fonts(cx);
    let theme_group = group(
        "Theme",
        [
            row(
                "Mode",
                None,
                segmented(
                    "mode",
                    Mode::ALL,
                    Mode::label,
                    look.mode,
                    setter(input, |look, mode| look.mode = mode),
                    None,
                    cx,
                ),
                cx,
            ),
            row(
                "Accent",
                Some("Buttons, focus rings and selection"),
                accents(
                    look.accent,
                    setter(input, |look, accent| look.accent = accent),
                    cx,
                ),
                cx,
            ),
        ],
        cx,
    );
    let window_group = group(
        "Window",
        [row(
            "Glass",
            Some(
                "How much of the desktop shows through. Always solid when Reduce Transparency is on.",
            ),
            segmented(
                "glass",
                Glass::ALL,
                Glass::label,
                look.glass,
                setter(input, |look, glass| look.glass = glass),
                None,
                cx,
            ),
            cx,
        )],
        cx,
    );
    let text_group = group(
        "Text",
        [
            row(
                "Interface font",
                None,
                font_menu(
                    "interface-font",
                    &look.interface_font,
                    FontMenu {
                        system: SYSTEM_SANS.into(),
                        default_family: None,
                        choices: INTERFACE_FONTS,
                    },
                    installed,
                    setter(input, |look, font| look.interface_font = font),
                ),
                cx,
            ),
            row(
                "Interface size",
                None,
                size_menu(
                    "interface-size",
                    look.interface_size.points(),
                    TextSize::choices().map(TextSize::points),
                    setter(input, |look, points| {
                        look.interface_size = TextSize::new(points)
                    }),
                ),
                cx,
            ),
            row(
                "Code font",
                None,
                font_menu(
                    "code-font",
                    &look.code_font,
                    FontMenu {
                        system: default_mono(),
                        default_family: Some(default_mono()),
                        choices: CODE_FONTS,
                    },
                    installed,
                    setter(input, |look, font| look.code_font = font),
                ),
                cx,
            ),
            row(
                "Code size",
                None,
                size_menu(
                    "code-size",
                    look.code_size.points(),
                    CodeSize::choices().map(CodeSize::points),
                    setter(input, |look, points| look.code_size = CodeSize::new(points)),
                ),
                cx,
            ),
        ],
        cx,
    );
    let conversation_group = group(
        "Conversation",
        [
            row(
                "Chat width",
                None,
                segmented(
                    "chat-width",
                    ChatWidth::ALL,
                    ChatWidth::label,
                    look.chat_width,
                    setter(input, |look, width| look.chat_width = width),
                    None,
                    cx,
                ),
                cx,
            ),
            row(
                "Diff colors",
                None,
                segmented(
                    "diff-colors",
                    DiffColors::ALL,
                    DiffColors::label,
                    look.diff_colors,
                    setter(input, |look, colors| look.diff_colors = colors),
                    Some(diff_dots),
                    cx,
                ),
                cx,
            ),
        ],
        cx,
    );
    let on_change = input.on_change.clone();
    let reset = Button::new("reset-appearance")
        .ghost()
        .small()
        .icon(IconName::RotateCcw)
        .label("Reset to defaults")
        .disabled(*look == Appearance::default())
        .debug_selector(|| "reset-appearance".to_string())
        .on_click(move |_, window, cx| on_change(Appearance::default(), window, cx));
    v_flex()
        .id("settings-scroll")
        .debug_selector(|| "settings-view".to_string())
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .p_4()
        .child(
            v_flex()
                .w_full()
                .max_w(px(COLUMN))
                .mx_auto()
                .gap_5()
                .child(theme_group)
                .child(window_group)
                .child(text_group)
                .child(preview(cx))
                .child(conversation_group)
                .child(h_flex().justify_end().pb_4().child(reset)),
        )
        .into_any_element()
}

/// A change of one choice: the current look with `edit` applied, handed to `on_change`.
fn setter<T: 'static>(
    input: &Input,
    edit: impl Fn(&mut Appearance, T) + 'static,
) -> impl Fn(T, &mut Window, &mut App) + Clone + 'static {
    let current = input.appearance.clone();
    let on_change = input.on_change.clone();
    let edit = Rc::new(edit);
    move |value, window, cx| {
        let mut next = current.clone();
        edit(&mut next, value);
        on_change(next, window, cx);
    }
}

/// A label above a panel of rows.
fn group<const N: usize>(label: &'static str, rows: [gpui::Div; N], cx: &App) -> AnyElement {
    let material = material(cx);
    v_flex()
        .gap_1p5()
        .child(
            div()
                .px_1()
                .text_sm()
                .text_color(material.glass_muted)
                .child(label),
        )
        .child(
            v_flex()
                .rounded_xl()
                .border_1()
                .border_color(material.card_stroke)
                .bg(material.glass_hover)
                .overflow_hidden()
                .children(rows.into_iter().enumerate().map(|(ix, row)| {
                    row.when(ix > 0, |row| {
                        row.border_t_1().border_color(material.card_stroke)
                    })
                })),
        )
        .into_any_element()
}

/// One setting: its name (and what it does) on the left, its control on the right.
fn row(
    title: &'static str,
    detail: Option<&'static str>,
    control: AnyElement,
    cx: &App,
) -> gpui::Div {
    h_flex()
        .items_center()
        .justify_between()
        .gap_4()
        .px_3()
        .py_2p5()
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(div().child(title))
                .children(detail.map(|detail| {
                    div()
                        .text_sm()
                        .text_color(material(cx).glass_muted)
                        .child(detail)
                })),
        )
        .child(div().flex_none().child(control))
}

/// A row of choices where exactly one is on. `adorn` draws something before a label.
fn segmented<T: Copy + PartialEq + 'static>(
    id: &'static str,
    options: &'static [T],
    label: fn(T) -> &'static str,
    selected: T,
    set: impl Fn(T, &mut Window, &mut App) + Clone + 'static,
    adorn: Option<fn(T) -> AnyElement>,
    cx: &App,
) -> AnyElement {
    let material = material(cx);
    h_flex()
        .id(id)
        .gap_0p5()
        .p_0p5()
        .rounded_lg()
        .bg(material.glass_selected)
        .children(options.iter().copied().map(move |option| {
            let set = set.clone();
            let name = label(option);
            Button::new(SharedString::from(format!("{id}-{name}")))
                .ghost()
                .small()
                .selected(option == selected)
                .when(option == selected, |button| {
                    button
                        .bg(material.card)
                        .border_1()
                        .border_color(material.card_stroke)
                })
                .debug_selector(move || format!("{id}-{name}"))
                .child(
                    h_flex()
                        .gap_1p5()
                        .items_center()
                        .children(adorn.map(|adorn| adorn(option)))
                        .child(name),
                )
                .on_click(move |_, window, cx| set(option, window, cx))
        }))
        .into_any_element()
}

fn dot(color: Hsla) -> gpui::Div {
    div().size(px(7.)).rounded_full().bg(color)
}

fn diff_dots(colors: DiffColors) -> AnyElement {
    let (added, removed): (Hsla, Hsla) = match colors {
        DiffColors::GreenRed => (gpui::rgb(0x22c55e).into(), gpui::rgb(0xef4444).into()),
        DiffColors::BlueOrange => (gpui::rgb(0x3b82f6).into(), gpui::rgb(0xf97316).into()),
    };
    h_flex()
        .gap_0p5()
        .child(dot(added))
        .child(dot(removed))
        .into_any_element()
}

/// The accent swatches. The chosen one wears a ring.
fn accents(
    selected: Accent,
    set: impl Fn(Accent, &mut Window, &mut App) + Clone + 'static,
    cx: &App,
) -> AnyElement {
    let ring = cx.theme().foreground;
    h_flex()
        .gap_2p5()
        .children(Accent::ALL.iter().copied().map(move |accent| {
            let set = set.clone();
            let on = accent == selected;
            div()
                .id(SharedString::from(format!("accent-{}", accent.label())))
                .debug_selector(move || format!("accent-{}", accent.label()))
                .flex_none()
                .size(px(SWATCH + 8.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .border_2()
                .border_color(if on { ring } else { gpui::transparent_black() })
                .cursor_pointer()
                .tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(accent.label()).build(window, cx)
                })
                .child(
                    div()
                        .size(px(SWATCH))
                        .rounded_full()
                        .bg(theme::accent_swatch(accent)),
                )
                .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                .on_click(move |_, window, cx| set(accent, window, cx))
        }))
        .into_any_element()
}

struct FontMenu {
    /// What the default is called.
    system: SharedString,
    /// The family the default names, which then is not listed a second time.
    default_family: Option<SharedString>,
    choices: &'static [&'static str],
}

/// A button showing the font in use, and a menu of the system's own and the installed ones.
fn font_menu(
    id: &'static str,
    current: &Font,
    menu: FontMenu,
    installed: &'static [String],
    set: impl Fn(Font, &mut Window, &mut App) + Clone + 'static,
) -> AnyElement {
    let shown: SharedString = match current {
        Font::System => menu.system.clone(),
        Font::Named(name) => name.clone().into(),
    };
    let mut names: Vec<String> = menu
        .choices
        .iter()
        .filter(|name| installed.iter().any(|family| family == *name))
        .filter(|name| menu.default_family.as_deref() != Some(**name))
        .map(|name| name.to_string())
        .collect();
    if let Font::Named(name) = current
        && !names.contains(name)
    {
        names.push(name.clone());
    }
    let current = current.clone();
    dropdown(id, shown.clone(), move |popup| {
        let system = {
            let set = set.clone();
            PopupMenuItem::new(menu.system.clone())
                .checked(current == Font::System)
                .on_click(move |_, window, cx| set(Font::System, window, cx))
        };
        names
            .iter()
            .fold(popup.item(system).separator(), |popup, name| {
                let (set, chosen) = (set.clone(), Font::Named(name.clone()));
                let family = SharedString::from(name.clone());
                popup.item(
                    PopupMenuItem::element(move |_, _| {
                        div().font_family(family.clone()).child(family.clone())
                    })
                    .checked(current == chosen)
                    .on_click(move |_, window, cx| set(chosen.clone(), window, cx)),
                )
            })
    })
}

fn default_mono() -> SharedString {
    TypographyTokens::default().mono
}

fn size_menu(
    id: &'static str,
    current: u8,
    sizes: impl Iterator<Item = u8>,
    set: impl Fn(u8, &mut Window, &mut App) + Clone + 'static,
) -> AnyElement {
    let sizes: Vec<u8> = sizes.collect();
    dropdown(id, format!("{current} pt").into(), move |popup| {
        sizes.iter().fold(popup, |popup, &points| {
            let set = set.clone();
            popup.item(
                PopupMenuItem::new(format!("{points} pt"))
                    .checked(points == current)
                    .on_click(move |_, window, cx| set(points, window, cx)),
            )
        })
    })
}

/// An outlined button with a caret that opens `menu`, aligned to its right edge.
fn dropdown(
    id: &'static str,
    shown: SharedString,
    menu: impl Fn(gpui_kit::component::menu::PopupMenu) -> gpui_kit::component::menu::PopupMenu
    + 'static,
) -> AnyElement {
    Button::new(id)
        .outline()
        .small()
        .min_w(px(MENU_WIDTH - 20.))
        .label(shown.clone())
        .dropdown_caret(true)
        .accessibility_label(shown)
        .debug_selector(move || id.to_string())
        .dropdown_menu_with_anchor(Anchor::TopRight, move |popup, _, _| {
            menu(popup.min_w(px(MENU_WIDTH)).max_h(px(320.)).scrollable(true))
        })
        .into_any_element()
}

/// The words as they now look: the interface font and size above the code font and size.
fn preview(cx: &App) -> AnyElement {
    let material = material(cx);
    let theme = cx.theme();
    v_flex()
        .gap_1()
        .px_3()
        .py_2p5()
        .rounded_xl()
        .border_1()
        .border_color(material.card_stroke)
        .bg(material.glass_hover)
        .debug_selector(|| "text-preview".to_string())
        .child(
            div()
                .font_weight(FontWeight::NORMAL)
                .child("Fix the flaky orphan test"),
        )
        .child(
            div()
                .font_family(theme.mono_font_family.clone())
                .text_size(theme.mono_font_size)
                .text_color(material.glass_muted)
                .child("cargo test -p newjen-core"),
        )
        .into_any_element()
}
