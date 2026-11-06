//! Bounded desktop preferences shared by the compositor, shell and Settings launcher.
use serde_json::{Value, json};
use std::path::PathBuf;

pub fn defaults() -> Value {
    json!({"scheme":"dark","accent":"blue","icon_theme":"","cursor_theme":"",
        "cursor_size":24,"font_size":14,"dock_follow":true,
        "high_contrast":false,"visual_alerts":false,"sticky_keys":false,"swipe_moves":false,
        "notifications":true,"dnd":false,"history":true,"blocked_apps":[],
        "sounds":false,"sound_theme":"freedesktop","sound_volume":40,
        "sound_events":["notification","warning"],
        "keyboard_layout":"us","keyboard_variant":"","keyboard_options":"",
        "left_handed":false,"flat_acceleration":false,"disable_while_typing":true,
        "move_modifier":"alt","expand_modifier":"super",
        "screen_off_minutes":0,"sleep_minutes":0,"lid_action":"lock",
        "push_enabled":false,"push_provider":"Ntfy","push_url":"https://ntfy.sh"})
}

pub fn path() -> Option<PathBuf> {
    Some(
        crate::files::xdg_dir(
            std::env::var_os("XDG_CONFIG_HOME").as_deref(),
            std::env::var_os("HOME").as_deref(),
            ".config",
        )?
        .join("navi/desktop.json"),
    )
}

fn name(value: &str) -> bool {
    value.len() <= 128
        && !value.contains(['/', '\\'])
        && value != "."
        && value != ".."
        && !value.chars().any(char::is_control)
}

pub fn parse(value: &Value) -> Result<Value, String> {
    let mut result = defaults();
    let input = value
        .as_object()
        .ok_or("Desktop settings must be an object")?;
    for (key, value) in input {
        let valid = match key.as_str() {
            "scheme" => matches!(value.as_str(), Some("dark" | "light")),
            "accent" => matches!(
                value.as_str(),
                Some("blue" | "green" | "purple" | "amber" | "pink")
            ),
            "icon_theme" | "cursor_theme" | "sound_theme" => value.as_str().is_some_and(name),
            "cursor_size" => value.as_u64().is_some_and(|n| (16..=64).contains(&n)),
            "font_size" => value.as_u64().is_some_and(|n| (12..=24).contains(&n)),
            "sound_volume" => value.as_u64().is_some_and(|n| n <= 100),
            "screen_off_minutes" | "sleep_minutes" => value.as_u64().is_some_and(|n| n <= 240),
            "move_modifier" | "expand_modifier" => matches!(value.as_str(), Some("alt" | "super")),
            "lid_action" => matches!(value.as_str(), Some("nothing" | "lock" | "suspend")),
            "push_provider" => matches!(value.as_str(), Some("Ntfy" | "Autopush")),
            "push_url" => value.as_str().is_some_and(|s| {
                s.len() <= 1000
                    && s.starts_with("https://")
                    && s.len() > 8
                    && !s.chars().any(|c| c.is_whitespace() || c.is_control())
                    && !s.contains(['@', '#', '?', '\\'])
            }),
            "keyboard_layout" | "keyboard_variant" | "keyboard_options" => {
                value.as_str().is_some_and(|s| {
                    s.len() <= 128
                        && s.chars()
                            .all(|c| c.is_ascii_alphanumeric() || "_-,():".contains(c))
                })
            }
            "blocked_apps" => value.as_array().is_some_and(|a| {
                a.len() <= 128
                    && a.iter().all(|v| {
                        v.as_str().is_some_and(|s| {
                            !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control)
                        })
                    })
            }),
            "sound_events" => value.as_array().is_some_and(|a| {
                a.len() <= 6
                    && a.iter().all(|v| {
                        matches!(
                            v.as_str(),
                            Some(
                                "notification"
                                    | "warning"
                                    | "volume"
                                    | "battery"
                                    | "login"
                                    | "logout"
                            )
                        )
                    })
            }),
            _ if result.get(key).is_some_and(Value::is_boolean) => value.is_boolean(),
            _ => false,
        };
        if !valid {
            return Err(format!("Invalid desktop setting: {key}"));
        }
        result[key] = value.clone();
    }
    if result["move_modifier"] == result["expand_modifier"] {
        return Err("Move and expand must use different modifiers".into());
    }
    Ok(result)
}

pub fn load() -> Result<Value, String> {
    let path = path().ok_or("No configuration directory")?;
    let Some(bytes) = crate::files::read_bytes(&path, 16384)? else {
        return Ok(defaults());
    };
    parse(
        &serde_json::from_slice::<Value>(&bytes)
            .map_err(|_| "Repair desktop.json before saving settings")?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_desktop_settings_and_migrates_missing_fields() {
        assert_eq!(parse(&json!({})).unwrap(), defaults());
        for v in [
            json!({"push_enabled":"yes"}),
            json!({"push_url":"http://host"}),
            json!({"cursor_theme":"../escape"}),
            json!({"sound_volume":101}),
            json!({"move_modifier":"super"}),
            json!({"keyboard_options":";command"}),
            json!({"unknown":true}),
        ] {
            assert!(parse(&v).is_err(), "{v}");
        }
        assert_eq!(
            parse(&json!({"push_enabled":true})).unwrap()["push_enabled"],
            true
        );
    }
}
