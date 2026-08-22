//! Black / white app theme (used on dashboard shell, not landing).
//! Persisted in `localStorage` as `fleet-theme` (`black` | `white`).

use dioxus::prelude::*;

const STORAGE_KEY: &str = "fleet-theme";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    #[default]
    Black,
    White,
}

impl Theme {
    #[allow(dead_code)]
    pub fn as_storage(self) -> &'static str {
        match self {
            Theme::Black => "black",
            Theme::White => "white",
        }
    }

    #[allow(dead_code)]
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
                && let Ok(Some(raw)) = storage.get_item(STORAGE_KEY)
            {
                return Self::from_storage(&raw);
            }
        }
        Theme::Black
    }

    pub fn persist(self) {
        #[cfg(target_arch = "wasm32")]
        {
            if let Some(window) = web_sys::window()
                && let Ok(Some(storage)) = window.local_storage()
            {
                let _ = storage.set_item(STORAGE_KEY, self.as_storage());
            }
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_storage_maps_aliases() {
        assert_eq!(Theme::from_storage("white"), Theme::White);
        assert_eq!(Theme::from_storage("Light"), Theme::White);
        assert_eq!(Theme::from_storage("black"), Theme::Black);
        assert_eq!(Theme::from_storage(""), Theme::Black);
    }
}
