//! System-locale and timezone heuristics for pre-filling Azure Support
//! contact fields (`country`, `preferredSupportLanguage`, `preferredTimeZone`)
//! so we don't blank-ask the user for facts the OS already knows.
//!
//! Everything is best-effort and side-effect-free; on any failure we return
//! `None` for that field and the client falls back to asking the user.

use std::env;
use std::path::PathBuf;

#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
pub struct LocaleHints {
    /// ISO 3166-1 alpha-3 (Azure Support uses 3-letter codes, e.g. `USA`).
    pub country: Option<String>,
    /// BCP-47-ish culture code, lowercased with hyphen, e.g. `en-us`.
    pub preferred_support_language: Option<String>,
    /// Microsoft TimeZoneInfo display name (what Azure Support accepts),
    /// e.g. `Pacific Standard Time`.
    pub preferred_time_zone: Option<String>,
}

pub fn detect() -> LocaleHints {
    let raw_locale = first_env(&["LC_ALL", "LC_MESSAGES", "LANG"]);
    let (lang2, country2) = parse_locale(raw_locale.as_deref());

    let preferred_support_language = match (&lang2, &country2) {
        (Some(l), Some(c)) => Some(format!("{}-{}", l.to_lowercase(), c.to_lowercase())),
        (Some(l), None) => Some(l.to_lowercase()),
        _ => None,
    };

    let country = country2
        .as_deref()
        .and_then(alpha2_to_alpha3)
        .map(str::to_string);

    let preferred_time_zone =
        detect_system_iana_tz().and_then(|iana| iana_to_windows_tz(&iana).map(str::to_string));

    LocaleHints {
        country,
        preferred_support_language,
        preferred_time_zone,
    }
}

fn first_env(keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Ok(v) = env::var(k) {
            if !v.is_empty() && v != "C" && v != "POSIX" {
                return Some(v);
            }
        }
    }
    None
}

/// Parse strings like `en_US.UTF-8`, `en-GB`, `de_DE`. Returns (lang, country).
fn parse_locale(s: Option<&str>) -> (Option<String>, Option<String>) {
    let Some(s) = s else { return (None, None) };
    let head = s.split('.').next().unwrap_or(s);
    let head = head.split('@').next().unwrap_or(head);
    let mut parts = head.split(['_', '-']);
    let lang = parts
        .next()
        .filter(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_alphabetic()))
        .map(str::to_string);
    let country = parts
        .next()
        .filter(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_alphabetic()))
        .map(|s| s.to_ascii_uppercase());
    (lang, country)
}

/// Read `/etc/localtime` (Unix), follow symlink, return IANA tz like
/// `America/Los_Angeles`.
fn detect_system_iana_tz() -> Option<String> {
    if let Ok(v) = env::var("TZ") {
        if !v.is_empty() && v.contains('/') {
            return Some(v);
        }
    }
    let p = PathBuf::from("/etc/localtime");
    let target = std::fs::read_link(&p).ok()?;
    let s = target.to_string_lossy().into_owned();
    // Common prefixes:
    //   /usr/share/zoneinfo/America/Los_Angeles
    //   /var/db/timezone/zoneinfo/America/Los_Angeles
    for marker in ["/zoneinfo/", "/zoneinfo.default/"] {
        if let Some(idx) = s.find(marker) {
            return Some(s[idx + marker.len()..].to_string());
        }
    }
    None
}

/// Subset of ISO 3166-1 alpha-2 → alpha-3 for the ~60 most common countries
/// where Azure Support is actually used. If the user is somewhere else we'd
/// rather return `None` and ask than guess wrong.
pub fn alpha2_to_alpha3(code: &str) -> Option<&'static str> {
    let c = code.to_ascii_uppercase();
    Some(match c.as_str() {
        "AE" => "ARE",
        "AR" => "ARG",
        "AT" => "AUT",
        "AU" => "AUS",
        "BE" => "BEL",
        "BG" => "BGR",
        "BH" => "BHR",
        "BR" => "BRA",
        "CA" => "CAN",
        "CH" => "CHE",
        "CL" => "CHL",
        "CN" => "CHN",
        "CO" => "COL",
        "CZ" => "CZE",
        "DE" => "DEU",
        "DK" => "DNK",
        "EE" => "EST",
        "EG" => "EGY",
        "ES" => "ESP",
        "FI" => "FIN",
        "FR" => "FRA",
        "GB" => "GBR",
        "GR" => "GRC",
        "HK" => "HKG",
        "HR" => "HRV",
        "HU" => "HUN",
        "ID" => "IDN",
        "IE" => "IRL",
        "IL" => "ISR",
        "IN" => "IND",
        "IS" => "ISL",
        "IT" => "ITA",
        "JP" => "JPN",
        "KR" => "KOR",
        "KW" => "KWT",
        "LT" => "LTU",
        "LU" => "LUX",
        "LV" => "LVA",
        "MX" => "MEX",
        "MY" => "MYS",
        "NG" => "NGA",
        "NL" => "NLD",
        "NO" => "NOR",
        "NZ" => "NZL",
        "PE" => "PER",
        "PH" => "PHL",
        "PK" => "PAK",
        "PL" => "POL",
        "PT" => "PRT",
        "QA" => "QAT",
        "RO" => "ROU",
        "RU" => "RUS",
        "SA" => "SAU",
        "SE" => "SWE",
        "SG" => "SGP",
        "SI" => "SVN",
        "SK" => "SVK",
        "TH" => "THA",
        "TR" => "TUR",
        "TW" => "TWN",
        "UA" => "UKR",
        "UK" => "GBR",
        "US" => "USA",
        "VN" => "VNM",
        "ZA" => "ZAF",
        _ => return None,
    })
}

/// Map IANA tz → Microsoft TimeZoneInfo display name. Covers the most
/// commonly used Azure Support customer zones; unknown zones return `None`
/// (caller will ask the user).
pub fn iana_to_windows_tz(iana: &str) -> Option<&'static str> {
    Some(match iana {
        // North America
        "America/Los_Angeles" | "America/Vancouver" | "America/Tijuana" => "Pacific Standard Time",
        "America/Denver" | "America/Edmonton" | "America/Boise" => "Mountain Standard Time",
        "America/Phoenix" => "US Mountain Standard Time",
        "America/Chicago" | "America/Mexico_City" | "America/Winnipeg" => "Central Standard Time",
        "America/New_York" | "America/Toronto" | "America/Detroit" | "America/Montreal" => {
            "Eastern Standard Time"
        }
        "America/Indiana/Indianapolis" => "US Eastern Standard Time",
        "America/Halifax" => "Atlantic Standard Time",
        "America/St_Johns" => "Newfoundland Standard Time",
        "America/Anchorage" => "Alaskan Standard Time",
        "Pacific/Honolulu" => "Hawaiian Standard Time",
        // South America
        "America/Sao_Paulo" => "E. South America Standard Time",
        "America/Buenos_Aires" | "America/Argentina/Buenos_Aires" => "Argentina Standard Time",
        "America/Santiago" => "Pacific SA Standard Time",
        "America/Bogota" | "America/Lima" => "SA Pacific Standard Time",
        // Europe / Africa
        "Etc/UTC" | "UTC" | "Etc/GMT" => "UTC",
        "Europe/London" | "Europe/Dublin" | "Europe/Lisbon" => "GMT Standard Time",
        "Europe/Berlin" | "Europe/Paris" | "Europe/Madrid" | "Europe/Rome" | "Europe/Amsterdam"
        | "Europe/Brussels" | "Europe/Vienna" | "Europe/Zurich" | "Europe/Prague"
        | "Europe/Warsaw" | "Europe/Stockholm" | "Europe/Copenhagen" | "Europe/Oslo"
        | "Europe/Budapest" | "Europe/Belgrade" => "W. Europe Standard Time",
        "Europe/Athens" | "Europe/Bucharest" | "Europe/Helsinki" | "Europe/Kiev"
        | "Europe/Riga" | "Europe/Tallinn" | "Europe/Vilnius" | "Africa/Cairo" => {
            "GTB Standard Time"
        }
        "Europe/Istanbul" => "Turkey Standard Time",
        "Europe/Moscow" => "Russian Standard Time",
        "Africa/Johannesburg" => "South Africa Standard Time",
        "Africa/Lagos" => "W. Central Africa Standard Time",
        // Middle East / Asia
        "Asia/Dubai" | "Asia/Muscat" => "Arabian Standard Time",
        "Asia/Riyadh" | "Asia/Qatar" | "Asia/Kuwait" | "Asia/Bahrain" => "Arab Standard Time",
        "Asia/Jerusalem" => "Israel Standard Time",
        "Asia/Karachi" => "Pakistan Standard Time",
        "Asia/Kolkata" | "Asia/Calcutta" => "India Standard Time",
        "Asia/Bangkok" | "Asia/Jakarta" | "Asia/Ho_Chi_Minh" => "SE Asia Standard Time",
        "Asia/Singapore" | "Asia/Kuala_Lumpur" | "Asia/Manila" | "Asia/Hong_Kong"
        | "Asia/Taipei" => "Singapore Standard Time",
        "Asia/Shanghai" | "Asia/Beijing" => "China Standard Time",
        "Asia/Tokyo" => "Tokyo Standard Time",
        "Asia/Seoul" => "Korea Standard Time",
        // Oceania
        "Australia/Perth" => "W. Australia Standard Time",
        "Australia/Adelaide" => "Cen. Australia Standard Time",
        "Australia/Sydney" | "Australia/Melbourne" | "Australia/Brisbane" | "Australia/Hobart" => {
            "AUS Eastern Standard Time"
        }
        "Pacific/Auckland" => "New Zealand Standard Time",
        _ => return None,
    })
}

/// Split a display `name` claim like "Alice Q. Example" into (first, last).
/// Matches Azure Support / slack-bot semantics: first token → first_name,
/// **everything after the first space → last_name** (so compound surnames
/// like "Maria del Carmen Garcia" stay intact: last = "del Carmen Garcia").
/// Returns `None` if there's only one token.
pub fn split_display_name(name: &str) -> Option<(String, String)> {
    let trimmed = name.trim();
    let (first, rest) = trimmed.split_once(char::is_whitespace)?;
    let rest = rest.trim();
    if first.is_empty() || rest.is_empty() {
        return None;
    }
    Some((first.to_string(), rest.to_string()))
}

/// Strip characters Azure Support rejects from contact names. Mirrors the
/// existing `azure-support-slack-bot` rule (`[^A-Za-z\s\-']`) which has been
/// empirically validated against the Azure REST API: accented Unicode like
/// "André" / "Müller" causes a 400, so we strip to ASCII letters + space +
/// hyphen + apostrophe. Returns `None` if the sanitized result is empty
/// (caller should fall back to asking the user).
pub fn sanitize_contact_name(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphabetic() || c.is_ascii_whitespace() || *c == '-' || *c == '\'')
        .collect();
    let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_unix_locale() {
        assert_eq!(
            parse_locale(Some("en_US.UTF-8")),
            (Some("en".into()), Some("US".into()))
        );
        assert_eq!(
            parse_locale(Some("de-DE")),
            (Some("de".into()), Some("DE".into()))
        );
        assert_eq!(
            parse_locale(Some("ja_JP")),
            (Some("ja".into()), Some("JP".into()))
        );
        assert_eq!(parse_locale(Some("en")), (Some("en".into()), None));
        assert_eq!(parse_locale(Some("C")), (None, None));
        assert_eq!(parse_locale(None), (None, None));
    }

    #[test]
    fn alpha2_to_alpha3_known() {
        assert_eq!(alpha2_to_alpha3("US"), Some("USA"));
        assert_eq!(alpha2_to_alpha3("gb"), Some("GBR"));
        assert_eq!(alpha2_to_alpha3("DE"), Some("DEU"));
        assert_eq!(alpha2_to_alpha3("XX"), None);
    }

    #[test]
    fn iana_to_windows_known() {
        assert_eq!(
            iana_to_windows_tz("America/Los_Angeles"),
            Some("Pacific Standard Time")
        );
        assert_eq!(
            iana_to_windows_tz("Europe/Berlin"),
            Some("W. Europe Standard Time")
        );
        assert_eq!(
            iana_to_windows_tz("Asia/Tokyo"),
            Some("Tokyo Standard Time")
        );
        assert_eq!(iana_to_windows_tz("Mars/Olympus_Mons"), None);
    }

    #[test]
    fn split_name_basic() {
        assert_eq!(
            split_display_name("Alice Example"),
            Some(("Alice".into(), "Example".into()))
        );
        assert_eq!(
            split_display_name("Alice Q. Example"),
            Some(("Alice".into(), "Q. Example".into()))
        );
        assert_eq!(
            split_display_name("Maria del Carmen Garcia"),
            Some(("Maria".into(), "del Carmen Garcia".into()))
        );
        assert_eq!(split_display_name("Cher"), None);
        assert_eq!(split_display_name("  "), None);
    }

    #[test]
    fn sanitize_strips_unicode_and_punct() {
        // Accented chars dropped.
        assert_eq!(sanitize_contact_name("André"), Some("Andr".into()));
        assert_eq!(sanitize_contact_name("Müller"), Some("Mller".into()));
        // Hyphen and apostrophe kept.
        assert_eq!(
            sanitize_contact_name("O'Brien-Smith"),
            Some("O'Brien-Smith".into())
        );
        // Whitespace collapsed.
        assert_eq!(
            sanitize_contact_name("  Maria   del   Carmen "),
            Some("Maria del Carmen".into())
        );
        // Digits + symbols stripped.
        assert_eq!(sanitize_contact_name("Alice42!"), Some("Alice".into()));
        // Empty after strip → None.
        assert_eq!(sanitize_contact_name("123!@#"), None);
        assert_eq!(sanitize_contact_name(""), None);
    }
}
