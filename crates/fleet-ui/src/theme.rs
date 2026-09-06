//! Black / white app theme (used on dashboard shell, not landing).
//! Persisted in `localStorage` as `fleet-theme` (`black` | `white`).

use dioxus::prelude::*;



#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    #[default]
    Black,
    White,
}

impl Theme {
    pub fn as_storage(self) -> &'static str {
        match self {
            Theme::Black => "black",
            Theme::White => "white",
        }
    }

    pub fn from_storage(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "white" | "light" => Theme::White,
            _ => Theme::Black,
        }
    }

    pub fn load_saved() -> Self {
        #[cfg(target_arch = "wasm32")]
        {
            if let Some(window) = web_sys::window()
                && let Ok(Some(storage)) = window.local_storage()
                && let Ok(Some(raw)) = storage.get_item("fleet-theme")
            {
                return Self::from_storage(&raw);
            }
        }
        Self::from_storage("")
    }

    pub fn persist(self) {
        #[cfg(target_arch = "wasm32")]
        {
            if let Some(window) = web_sys::window()
                && let Ok(Some(storage)) = window.local_storage()
            {
                let _ = storage.set_item("fleet-theme", self.as_storage());
            }
        }
    }

    pub fn root_class(self) -> &'static str {
        match self {
            Theme::Black => "theme-black",
            Theme::White => "theme-white",
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_storage_maps_aliases() {
        assert_eq!(Theme::from_storage("white"), Theme::White);
        assert_eq!(Theme::from_storage("light"), Theme::White);
        assert_eq!(Theme::Black.as_storage(), "black");
        assert_eq!(Theme::White.as_storage(), "white");
    }

    #[test]
    fn root_class_is_theme_black_or_white() {
        assert_eq!(Theme::Black.root_class(), "theme-black");
        assert_eq!(Theme::White.root_class(), "theme-white");
    }

    #[test]
    fn toggle_swaps_black_and_white() {
        assert_eq!(Theme::Black.toggle(), Theme::White);
        assert_eq!(Theme::White.toggle(), Theme::Black);
    }

    #[test]
    fn from_storage_trims_unknown_to_black() {
        assert_eq!(Theme::from_storage(" WHITE "), Theme::White);
        assert_eq!(Theme::from_storage(" Light "), Theme::White);
        assert_eq!(Theme::from_storage("dark"), Theme::Black);
        assert_eq!(Theme::from_storage("foo"), Theme::Black);
        assert_eq!(Theme::from_storage("  "), Theme::Black);
    }
}
