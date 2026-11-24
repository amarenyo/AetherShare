//! Cross-desktop language detection & localization for AetherShare.
//!
//! Detects the active user language across KDE Plasma (`~/.config/plasma-localerc`),
//! GNOME / COSMIC / XFCE / Hyprland / standard Linux (`LC_ALL`, `LC_MESSAGES`, `LANGUAGE`,
//! `LANG`, `~/.config/locale.conf`, `/etc/locale.conf`).
//!
//! Automatically selects German (`de`) for German locales and English (`en`) otherwise,
//! with optional runtime override from the Settings UI.

use std::sync::RwLock;

use once_cell::sync::Lazy;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    De,
    En,
}

impl Lang {
    pub fn code(self) -> &'static str {
        match self {
            Lang::De => "de",
            Lang::En => "en",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LangPref {
    System,
    De,
    En,
}

impl LangPref {
    pub fn from_str_lossy(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "de" | "german" | "deutsch" => LangPref::De,
            "en" | "english" | "englisch" => LangPref::En,
            _ => LangPref::System,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            LangPref::System => "system",
            LangPref::De => "de",
            LangPref::En => "en",
        }
    }
}

static LANG_PREF: Lazy<RwLock<LangPref>> = Lazy::new(|| RwLock::new(LangPref::System));

pub fn set_language_pref(pref: LangPref) {
    if let Ok(mut guard) = LANG_PREF.write() {
        *guard = pref;
    }
}

pub fn get_language_pref() -> LangPref {
    LANG_PREF
        .read()
        .map(|g| *g)
        .unwrap_or(LangPref::System)
}

pub fn active_lang() -> Lang {
    match get_language_pref() {
        LangPref::De => Lang::De,
        LangPref::En => Lang::En,
        LangPref::System => detect_system_lang(),
    }
}

/// Parses a single locale string (e.g. `"de_DE.UTF-8"`, `"en_US"`, `"de_AT@euro"`)
/// or a colon-separated `LANGUAGE` list (e.g. `"de_CH:de:en_US"`).
pub fn parse_locale_list(raw: &str) -> Option<Lang> {
    for entry in raw.split(':') {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        let primary = trimmed
            .split(['.', '@', '_', '-'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        match primary.as_str() {
            "de" => return Some(Lang::De),
            "en" => return Some(Lang::En),
            "c" | "posix" => continue,
            _ => continue,
        }
    }
    None
}

fn read_plasma_localerc_key(target_section: &str, key_prefix: &str) -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let contents = std::fs::read_to_string(format!("{home}/.config/plasma-localerc")).ok()?;
    let mut section = String::new();
    for line in contents.lines() {
        let t = line.trim();
        if t.starts_with('[') && t.ends_with(']') {
            section = t.to_string();
            continue;
        }
        if section == target_section {
            if let Some(val) = t.strip_prefix(key_prefix) {
                let v = val.trim().trim_matches('"').to_string();
                if !v.is_empty() {
                    return Some(v);
                }
            }
        }
    }
    None
}

fn read_locale_conf(path: &str) -> Option<Lang> {
    let contents = std::fs::read_to_string(path).ok()?;
    for key in ["LC_ALL=", "LC_MESSAGES=", "LANGUAGE=", "LANG="] {
        for line in contents.lines() {
            let t = line.trim();
            if let Some(val) = t.strip_prefix(key) {
                let clean = val.trim().trim_matches('"');
                if let Some(lang) = parse_locale_list(clean) {
                    return Some(lang);
                }
            }
        }
    }
    None
}

/// Detects the user's preferred language from KDE Plasma, GNOME, and standard Linux locale settings.
/// Defaults to English (`Lang::En`) when no German locale is detected.
pub fn detect_system_lang() -> Lang {
    // 1. KDE Plasma explicit UI language override (~/.config/plasma-localerc [Translations] LANGUAGE=)
    if let Some(val) = read_plasma_localerc_key("[Translations]", "LANGUAGE=") {
        if let Some(lang) = parse_locale_list(&val) {
            return lang;
        }
    }

    // 2. Standard POSIX / GNOME / KDE / COSMIC / XFCE / Hyprland environment variables
    for var in ["LC_ALL", "LC_MESSAGES", "LANGUAGE", "LANG"] {
        if let Ok(val) = std::env::var(var) {
            if let Some(lang) = parse_locale_list(&val) {
                return lang;
            }
        }
    }

    // 3. KDE Plasma Formats fallback (~/.config/plasma-localerc [Formats] LANG=)
    if let Some(val) = read_plasma_localerc_key("[Formats]", "LANG=") {
        if let Some(lang) = parse_locale_list(&val) {
            return lang;
        }
    }

    // 4. User / system locale.conf fallback
    if let Ok(home) = std::env::var("HOME") {
        if let Some(lang) = read_locale_conf(&format!("{home}/.config/locale.conf")) {
            return lang;
        }
    }
    if let Some(lang) = read_locale_conf("/etc/locale.conf") {
        return lang;
    }

    Lang::En
}

// ---- Localized Strings for Native Linux Notifications, Tray & Dialogs ----

pub struct NativeStrings {
    pub device_fallback: &'static str,
    pub files_fallback: &'static str,
    pub more_suffix: &'static str,
    pub text_prefix: &'static str,
    pub link_prefix: &'static str,
    pub wifi_prefix: &'static str,
    pub accept: &'static str,
    pub decline: &'static str,
    pub open: &'static str,
    pub incoming_title_prefix: &'static str,
    pub receiving_from: &'static str,
    pub sending_to: &'static str,
    pub received_from: &'static str,
    pub sent_to: &'static str,
    pub transfer_completed: &'static str,
    pub transfer_rejected: &'static str,
    pub transfer_cancelled: &'static str,
    pub field_device: &'static str,
    pub field_file: &'static str,
    pub tray_open: &'static str,
    pub tray_visible_all: &'static str,
    pub tray_invisible: &'static str,
    pub tray_quit: &'static str,
    pub dialog_pick_files: &'static str,
    pub dialog_pick_folder: &'static str,
}

const DE: NativeStrings = NativeStrings {
    device_fallback: "Gerät",
    files_fallback: "Dateien",
    more_suffix: "weitere",
    text_prefix: "Text",
    link_prefix: "Link",
    wifi_prefix: "WLAN",
    accept: "Annehmen",
    decline: "Ablehnen",
    open: "Öffnen",
    incoming_title_prefix: "Eingehender Transfer von",
    receiving_from: "Empfange von",
    sending_to: "Sende an",
    received_from: "Empfangen von",
    sent_to: "Gesendet an",
    transfer_completed: "Übertragung abgeschlossen",
    transfer_rejected: "Übertragung abgelehnt",
    transfer_cancelled: "Übertragung abgebrochen",
    field_device: "Gerät",
    field_file: "Datei",
    tray_open: "AetherShare öffnen",
    tray_visible_all: "Für alle sichtbar",
    tray_invisible: "Unsichtbar",
    tray_quit: "Beenden",
    dialog_pick_files: "Dateien mit AetherShare senden",
    dialog_pick_folder: "Ordner wählen",
};

const EN: NativeStrings = NativeStrings {
    device_fallback: "Device",
    files_fallback: "Files",
    more_suffix: "more",
    text_prefix: "Text",
    link_prefix: "Link",
    wifi_prefix: "Wi-Fi",
    accept: "Accept",
    decline: "Decline",
    open: "Open",
    incoming_title_prefix: "Incoming transfer from",
    receiving_from: "Receiving from",
    sending_to: "Sending to",
    received_from: "Received from",
    sent_to: "Sent to",
    transfer_completed: "Transfer completed",
    transfer_rejected: "Transfer declined",
    transfer_cancelled: "Transfer cancelled",
    field_device: "Device",
    field_file: "File",
    tray_open: "Open AetherShare",
    tray_visible_all: "Visible to everyone",
    tray_invisible: "Invisible",
    tray_quit: "Quit",
    dialog_pick_files: "Select files to send with AetherShare",
    dialog_pick_folder: "Select folder",
};

pub fn strings() -> &'static NativeStrings {
    match active_lang() {
        Lang::De => &DE,
        Lang::En => &EN,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_german_and_english_locales() {
        assert_eq!(parse_locale_list("de_DE.UTF-8"), Some(Lang::De));
        assert_eq!(parse_locale_list("de_CH:de:en_US"), Some(Lang::De));
        assert_eq!(parse_locale_list("en_US.UTF-8"), Some(Lang::En));
        assert_eq!(parse_locale_list("en_GB:de_DE"), Some(Lang::En));
        assert_eq!(parse_locale_list("C.UTF-8"), None);
        assert_eq!(parse_locale_list(""), None);
    }
}
