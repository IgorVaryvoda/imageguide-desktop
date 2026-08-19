//! Remember where the window was and what you were looking at.
//!
//! A tiny key=value file rather than a config crate. There are four values, and a
//! dependency that walks platform config directories costs more than the ten lines
//! it would save.

use std::path::PathBuf;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    pub width: Option<f32>,
    pub height: Option<f32>,
    pub folder: Option<PathBuf>,
}

/// Where the file lives on each platform. `None` when the environment gives us
/// nothing usable, in which case nothing is remembered and nothing breaks.
pub fn path() -> Option<PathBuf> {
    let base = if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .map(|home| home.join(".config"))
            })
    }?;

    Some(base.join("imageguide").join("settings"))
}

pub fn load() -> Settings {
    let Some(text) = path().and_then(|path| std::fs::read_to_string(path).ok()) else {
        return Settings::default();
    };
    parse(&text)
}

pub fn save(settings: &Settings) {
    let Some(path) = path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, render(settings));
}

fn parse(text: &str) -> Settings {
    let mut settings = Settings::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "width" => settings.width = value.trim().parse().ok(),
            "height" => settings.height = value.trim().parse().ok(),
            "folder" => settings.folder = Some(PathBuf::from(value.trim())),
            _ => {}
        }
    }
    settings
}

fn render(settings: &Settings) -> String {
    let mut out = String::new();
    if let Some(width) = settings.width {
        out.push_str(&format!("width={width}\n"));
    }
    if let Some(height) = settings.height {
        out.push_str(&format!("height={height}\n"));
    }
    if let Some(folder) = settings.folder.as_ref() {
        out.push_str(&format!("folder={}\n", folder.display()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_file_reads_back_the_same() {
        let settings = Settings {
            width: Some(1280.),
            height: Some(720.5),
            folder: Some(PathBuf::from("/home/igor/Pictures")),
        };
        assert_eq!(parse(&render(&settings)), settings);
    }

    /// A hand-edited or half-written file must not stop the app opening.
    #[test]
    fn nonsense_is_ignored_rather_than_fatal() {
        let settings = parse("width=not-a-number\nnokeyhere\n\nheight=600\n");
        assert_eq!(settings.width, None);
        assert_eq!(settings.height, Some(600.));
        assert_eq!(settings.folder, None);
    }

    #[test]
    fn a_folder_with_spaces_survives() {
        let settings = Settings {
            folder: Some(PathBuf::from("/home/igor/My Photos")),
            ..Settings::default()
        };
        assert_eq!(parse(&render(&settings)).folder, settings.folder);
    }
}
