//! How the user wants Newjen to look: a plain value the app applies and the store keeps.

use serde::{Deserialize, Serialize};

/// Every choice in Settings > Appearance. A field missing from the stored text takes its default,
/// so a version that adds a field still reads what an older one saved.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    pub mode: Mode,
    pub accent: Accent,
    pub glass: Glass,
    pub interface_font: Font,
    pub interface_size: TextSize,
    /// `Font::System` here means the mono font GPUI Kit ships.
    pub code_font: Font,
    pub code_size: CodeSize,
    pub chat_width: ChatWidth,
    pub diff_colors: DiffColors,
}

impl Appearance {
    /// Reads stored text. A preference that cannot be read is not worth failing a launch over, so
    /// anything unreadable gives the defaults.
    pub fn parse(text: &str) -> Self {
        serde_json::from_str(text).unwrap_or_default()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("appearance holds only plain values")
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    System,
    Light,
    Dark,
}

impl Mode {
    pub const ALL: &[Mode] = &[Mode::System, Mode::Light, Mode::Dark];

    pub fn label(self) -> &'static str {
        match self {
            Mode::System => "System",
            Mode::Light => "Light",
            Mode::Dark => "Dark",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Accent {
    #[default]
    Graphite,
    Blue,
    Indigo,
    Violet,
    Pink,
    Orange,
    Green,
}

impl Accent {
    pub const ALL: &[Accent] = &[
        Accent::Graphite,
        Accent::Blue,
        Accent::Indigo,
        Accent::Violet,
        Accent::Pink,
        Accent::Orange,
        Accent::Green,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Accent::Graphite => "Graphite",
            Accent::Blue => "Blue",
            Accent::Indigo => "Indigo",
            Accent::Violet => "Violet",
            Accent::Pink => "Pink",
            Accent::Orange => "Orange",
            Accent::Green => "Green",
        }
    }
}

/// How much of the desktop shows through the window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Glass {
    Clear,
    #[default]
    Frosted,
    Solid,
}

impl Glass {
    pub const ALL: &[Glass] = &[Glass::Clear, Glass::Frosted, Glass::Solid];

    pub fn label(self) -> &'static str {
        match self {
            Glass::Clear => "Clear",
            Glass::Frosted => "Frosted",
            Glass::Solid => "Solid",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Font {
    #[default]
    System,
    Named(String),
}

impl Font {
    /// The family to ask for, or `None` for the default one.
    pub fn family(&self) -> Option<&str> {
        match self {
            Font::System => None,
            Font::Named(name) => Some(name),
        }
    }
}

macro_rules! size {
    ($name:ident, $min:expr, $max:expr, $default:expr) => {
        /// A size in points. Out-of-range values are clamped on the way in, so none can exist.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(from = "u8", into = "u8")]
        pub struct $name(u8);

        impl $name {
            pub const MIN: u8 = $min;
            pub const MAX: u8 = $max;

            pub fn new(points: u8) -> Self {
                Self(points.clamp(Self::MIN, Self::MAX))
            }

            pub fn points(self) -> u8 {
                self.0
            }

            pub fn choices() -> impl Iterator<Item = Self> {
                (Self::MIN..=Self::MAX).map(Self)
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self($default)
            }
        }

        impl From<u8> for $name {
            fn from(points: u8) -> Self {
                Self::new(points)
            }
        }

        impl From<$name> for u8 {
            fn from(size: $name) -> u8 {
                size.0
            }
        }
    };
}

size!(TextSize, 12, 16, 14);
size!(CodeSize, 10, 16, 12);

/// How wide the conversation's column may grow.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatWidth {
    #[default]
    Comfortable,
    Wide,
    Full,
}

impl ChatWidth {
    pub const ALL: &[ChatWidth] = &[ChatWidth::Comfortable, ChatWidth::Wide, ChatWidth::Full];

    pub fn label(self) -> &'static str {
        match self {
            ChatWidth::Comfortable => "Comfortable",
            ChatWidth::Wide => "Wide",
            ChatWidth::Full => "Full",
        }
    }

    /// The column's widest size in points, or `None` for no cap.
    pub fn max_points(self) -> Option<u16> {
        match self {
            ChatWidth::Comfortable => Some(720),
            ChatWidth::Wide => Some(900),
            ChatWidth::Full => None,
        }
    }
}

/// The two colors diffs and statuses use for added and removed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffColors {
    #[default]
    GreenRed,
    BlueOrange,
}

impl DiffColors {
    pub const ALL: &[DiffColors] = &[DiffColors::GreenRed, DiffColors::BlueOrange];

    pub fn label(self) -> &'static str {
        match self {
            DiffColors::GreenRed => "Green / red",
            DiffColors::BlueOrange => "Blue / orange",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn changed() -> Appearance {
        Appearance {
            mode: Mode::Dark,
            accent: Accent::Violet,
            glass: Glass::Clear,
            interface_font: Font::Named("Helvetica Neue".into()),
            interface_size: TextSize::new(15),
            code_font: Font::Named("SF Mono".into()),
            code_size: CodeSize::new(13),
            chat_width: ChatWidth::Full,
            diff_colors: DiffColors::BlueOrange,
        }
    }

    #[test]
    fn the_defaults_are_the_look_before_settings_existed() {
        let defaults = Appearance::default();
        assert_eq!(defaults.mode, Mode::System);
        assert_eq!(defaults.accent, Accent::Graphite);
        assert_eq!(defaults.glass, Glass::Frosted);
        assert_eq!(defaults.interface_font, Font::System);
        assert_eq!(defaults.interface_size.points(), 14);
        assert_eq!(defaults.code_font, Font::System);
        assert_eq!(defaults.code_size.points(), 12);
        assert_eq!(defaults.chat_width.max_points(), Some(720));
        assert_eq!(defaults.diff_colors, DiffColors::GreenRed);
    }

    #[test]
    fn it_round_trips_through_json() {
        for appearance in [Appearance::default(), changed()] {
            assert_eq!(Appearance::parse(&appearance.to_json()), appearance);
        }
    }

    #[test]
    fn stored_names_are_snake_case() {
        let json = changed().to_json();
        assert!(json.contains(r#""diff_colors":"blue_orange""#), "{json}");
        assert!(json.contains(r#""interface_font":{"named":"Helvetica Neue"}"#));
        assert!(json.contains(r#""interface_size":15"#));
    }

    #[test]
    fn a_missing_field_takes_its_default() {
        let parsed = Appearance::parse(r#"{"accent":"pink"}"#);
        assert_eq!(parsed.accent, Accent::Pink);
        assert_eq!(
            Appearance {
                accent: Accent::Graphite,
                ..parsed
            },
            Appearance::default()
        );
    }

    #[test]
    fn sizes_clamp_into_their_range() {
        assert_eq!(TextSize::new(3).points(), TextSize::MIN);
        assert_eq!(TextSize::new(99).points(), TextSize::MAX);
        assert_eq!(CodeSize::new(0).points(), CodeSize::MIN);
        assert_eq!(CodeSize::new(200).points(), CodeSize::MAX);
        let parsed = Appearance::parse(r#"{"interface_size":200,"code_size":1}"#);
        assert_eq!(parsed.interface_size.points(), 16);
        assert_eq!(parsed.code_size.points(), 10);
        assert_eq!(
            TextSize::choices()
                .map(TextSize::points)
                .collect::<Vec<_>>(),
            [12, 13, 14, 15, 16]
        );
    }

    #[test]
    fn unreadable_text_gives_the_defaults() {
        for text in [
            "",
            "garbage",
            "null",
            r#"{"mode":"sepia"}"#,
            r#"{"accent":7}"#,
            r#"{"interface_size":"big"}"#,
            r#"{"interface_font":{"named":3}}"#,
        ] {
            assert_eq!(Appearance::parse(text), Appearance::default(), "{text}");
        }
    }

    #[test]
    fn every_choice_has_a_label_and_the_default_is_listed() {
        assert!(Mode::ALL.contains(&Mode::default()));
        assert!(Accent::ALL.contains(&Accent::default()));
        assert!(Glass::ALL.contains(&Glass::default()));
        assert!(ChatWidth::ALL.contains(&ChatWidth::default()));
        assert!(DiffColors::ALL.contains(&DiffColors::default()));
        assert_eq!(Accent::ALL.len(), 7);
        assert_eq!(DiffColors::BlueOrange.label(), "Blue / orange");
        assert_eq!(ChatWidth::Full.max_points(), None);
    }
}
