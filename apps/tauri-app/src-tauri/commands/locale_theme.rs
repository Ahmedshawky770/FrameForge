use crate::state::{AVAILABLE_LOCALES, LocaleInfo, LocaleState, ThemeInfo, ThemeState, locale_direction};
use std::sync::Mutex;
use tauri::{command, State};

#[command]
pub async fn locale_get(handle: State<'_, Mutex<LocaleState>>) -> Result<LocaleInfo, String> {
    let s = handle.lock().map_err(|e| e.to_string())?;
    Ok(LocaleInfo {
        current: s.current.clone(),
        available: AVAILABLE_LOCALES.iter().map(|s| s.to_string()).collect(),
        direction: locale_direction(&s.current).to_string(),
    })
}

#[command]
pub async fn locale_set(
    handle: State<'_, Mutex<LocaleState>>,
    locale: String,
) -> Result<LocaleInfo, String> {
    if !AVAILABLE_LOCALES.iter().any(|&l| l == locale) {
        return Err(format!("unsupported locale: {locale}"));
    }
    let mut s = handle.lock().map_err(|e| e.to_string())?;
    s.current = locale.clone();
    Ok(LocaleInfo {
        current: s.current.clone(),
        available: AVAILABLE_LOCALES.iter().map(|s| s.to_string()).collect(),
        direction: locale_direction(&s.current).to_string(),
    })
}

#[command]
pub async fn theme_get(handle: State<'_, Mutex<ThemeState>>) -> Result<ThemeInfo, String> {
    let s = handle.lock().map_err(|e| e.to_string())?;
    let effective = if s.current == "system" {
        // Default to dark on Linux if we can't detect; frontend will override via CSS media query.
        "dark".to_string()
    } else {
        s.current.clone()
    };
    Ok(ThemeInfo {
        current: s.current.clone(),
        effective,
    })
}

#[command]
pub async fn theme_set(
    handle: State<'_, Mutex<ThemeState>>,
    theme: String,
) -> Result<ThemeInfo, String> {
    if !matches!(theme.as_str(), "light" | "dark" | "system") {
        return Err(format!("unsupported theme: {theme}"));
    }
    let mut s = handle.lock().map_err(|e| e.to_string())?;
    s.current = theme.clone();
    let effective = if theme == "system" {
        "dark".to_string()
    } else {
        theme
    };
    Ok(ThemeInfo {
        current: s.current.clone(),
        effective,
    })
}