//! The one choice that is remembered between runs, the emulation mode, kept
//! as a small text file in %APPDATA%\protwo.

use std::path::PathBuf;

use crate::vigem::Kind;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Saved {
    pub kind: Kind,
}

impl Default for Saved {
    fn default() -> Self {
        Saved { kind: Kind::DualShock4 }
    }
}

fn path() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("protwo").join("settings.txt"))
}

/// Reads `name=value` lines; anything missing or unknown keeps its default.
fn parse(text: &str) -> Saved {
    let mut saved = Saved::default();
    for line in text.lines() {
        match line.trim().split_once('=') {
            Some(("mode", "xbox")) => saved.kind = Kind::Xbox360,
            Some(("mode", "ds4")) => saved.kind = Kind::DualShock4,
            _ => {}
        }
    }
    saved
}

fn format(saved: &Saved) -> String {
    format!("mode={}\n", if saved.kind == Kind::Xbox360 { "xbox" } else { "ds4" })
}

pub fn load() -> Saved {
    path().and_then(|p| std::fs::read_to_string(p).ok()).map_or_else(Saved::default, |text| parse(&text))
}

/// Best effort: a failure to save only means the choice is not remembered.
pub fn save(saved: &Saved) {
    if let Some(path) = path() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, format(saved));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip() {
        for kind in [Kind::DualShock4, Kind::Xbox360] {
            let saved = Saved { kind };
            assert_eq!(parse(&format(&saved)), saved);
        }
    }

    #[test]
    fn unknown_or_damaged_lines_keep_the_defaults() {
        assert_eq!(parse(""), Saved::default());
        assert_eq!(parse("mode=gamecube\nnonsense\n"), Saved::default());
        // Lines from older versions are ignored.
        assert_eq!(parse("mute=1\r\nbuttons=label\r\nmode=xbox\r\n").kind, Kind::Xbox360);
    }
}
