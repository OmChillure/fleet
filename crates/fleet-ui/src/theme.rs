//! Black / white app theme (used on dashboard shell, not landing).

use dioxus::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    #[default]
    Black,
    White,
}

impl Theme {
    pub fn root_class(self) -> &'static str {
        match self {
            Theme::Black => "theme-black",
            Theme::White => "theme-white",
        }
    }

    #[allow(dead_code)] // kept for accessibility / future text labels
    pub fn label(self) -> &'static str {
        match self {
            Theme::Black => "Black",
            Theme::White => "White",
        }
    }

    pub fn toggle(self) -> Self {
        match self {
            Theme::Black => Theme::White,
            Theme::White => Theme::Black,
        }
    }
}

pub type ThemeSignal = Signal<Theme>;

/// Read/write app theme from any component under `AppShell`.
pub fn use_theme() -> ThemeSignal {
    use_context::<ThemeSignal>()
}
