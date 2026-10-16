//! Texts in Spanish (default) and English. Each text is written in both languages where it is used:
//! `t("Guardar", "Save")` or `tr!("{n} colecciones", "{n} collections")`. No keys or separate files; the
//! compiler enforces that the translation exists. ponytail: with more languages, Fluent or rust-i18n would fit better.

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

use serde::{Deserialize, Serialize};

static EN: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    #[default]
    Es,
    En,
}

impl Language {
    /// "es" / "en" (or "english", "español", "EN"...).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().get(..2)? {
            "es" => Some(Language::Es),
            "en" => Some(Language::En),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Language::Es => "Español",
            Language::En => "English",
        }
    }
}

pub fn set(l: Language) {
    EN.store(l == Language::En, Relaxed);
}

pub fn current() -> Language {
    if en() { Language::En } else { Language::Es }
}

pub fn en() -> bool {
    EN.load(Relaxed)
}

/// Fixed text in the current language.
pub fn t(es: &'static str, en: &'static str) -> &'static str {
    if self::en() { en } else { es }
}

/// `MISHAPI_LANG=en` (or `es`) forces the language, e.g. in CI.
pub fn from_env() -> Option<Language> {
    std::env::var("MISHAPI_LANG").ok().as_deref().and_then(Language::parse)
}

/// Formatted text in the current language: `tr!("{n} colecciones", "{n} collections")`.
#[macro_export]
macro_rules! tr {
    ($es:literal, $en:literal $(, $arg:expr)* $(,)?) => {
        if $crate::i18n::en() { format!($en $(, $arg)*) } else { format!($es $(, $arg)*) }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_languages() {
        assert_eq!((Language::parse("EN"), Language::parse("español"), Language::parse("fr")), (Some(Language::En), Some(Language::Es), None));
        assert_eq!(Language::default(), Language::Es);
    }
}
