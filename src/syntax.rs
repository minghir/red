use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Cursor;
use std::sync::{Mutex, OnceLock};

use eframe::egui::{text::LayoutJob, Color32, FontId, Stroke, TextFormat};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Style, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

const GITHUB_DARK_TMTHEME: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>name</key>
    <string>GitHub Dark</string>
    <key>settings</key>
    <array>
        <dict>
            <key>settings</key>
            <dict>
                <key>background</key>
                <string>#0d1117</string>
                <key>caret</key>
                <string>#58a6ff</string>
                <key>foreground</key>
                <string>#e6edf3</string>
                <key>invisibles</key>
                <string>#484f58</string>
                <key>lineHighlight</key>
                <string>#161b22</string>
                <key>selection</key>
                <string>#264f78</string>
            </dict>
        </dict>
        <dict>
            <key>name</key>
            <string>Comment</string>
            <key>scope</key>
            <string>comment, punctuation.definition.comment</string>
            <key>settings</key>
            <dict>
                <key>fontStyle</key>
                <string>italic</string>
                <key>foreground</key>
                <string>#8b949e</string>
            </dict>
        </dict>
        <dict>
            <key>name</key>
            <string>Keyword and Storage</string>
            <key>scope</key>
            <string>keyword, storage, storage.type, storage.modifier</string>
            <key>settings</key>
            <dict>
                <key>foreground</key>
                <string>#ff7b72</string>
            </dict>
        </dict>
        <dict>
            <key>name</key>
            <string>String</string>
            <key>scope</key>
            <string>string, punctuation.definition.string</string>
            <key>settings</key>
            <dict>
                <key>foreground</key>
                <string>#a5d6ff</string>
            </dict>
        </dict>
        <dict>
            <key>name</key>
            <string>Constant and Number</string>
            <key>scope</key>
            <string>constant, constant.numeric, constant.language, constant.character</string>
            <key>settings</key>
            <dict>
                <key>foreground</key>
                <string>#79c0ff</string>
            </dict>
        </dict>
        <dict>
            <key>name</key>
            <string>Function</string>
            <key>scope</key>
            <string>entity.name.function, support.function, meta.function-call</string>
            <key>settings</key>
            <dict>
                <key>foreground</key>
                <string>#d2a8ff</string>
            </dict>
        </dict>
        <dict>
            <key>name</key>
            <string>Type and Struct</string>
            <key>scope</key>
            <string>entity.name.type, entity.name.class, entity.name.struct, entity.name.enum, entity.name.trait, support.type, support.class</string>
            <key>settings</key>
            <dict>
                <key>foreground</key>
                <string>#ffa657</string>
            </dict>
        </dict>
        <dict>
            <key>name</key>
            <string>Parameter</string>
            <key>scope</key>
            <string>variable.parameter</string>
            <key>settings</key>
            <dict>
                <key>foreground</key>
                <string>#ffa657</string>
            </dict>
        </dict>
        <dict>
            <key>name</key>
            <string>Tag and Macro</string>
            <key>scope</key>
            <string>entity.name.tag, support.macro, meta.macro</string>
            <key>settings</key>
            <dict>
                <key>foreground</key>
                <string>#7ee787</string>
            </dict>
        </dict>
        <dict>
            <key>name</key>
            <string>Invalid</string>
            <key>scope</key>
            <string>invalid</string>
            <key>settings</key>
            <dict>
                <key>foreground</key>
                <string>#f85149</string>
            </dict>
        </dict>
    </array>
</dict>
</plist>
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum EditorTheme {
    #[default]
    GitHubDark,
    OceanDark,
    EightiesDark,
    MochaDark,
    SolarizedDark,
    OceanLight,
    SolarizedLight,
    InspiredGitHub,
}

impl EditorTheme {
    pub const ALL: &'static [EditorTheme] = &[
        EditorTheme::GitHubDark,
        EditorTheme::OceanDark,
        EditorTheme::EightiesDark,
        EditorTheme::MochaDark,
        EditorTheme::SolarizedDark,
        EditorTheme::OceanLight,
        EditorTheme::SolarizedLight,
        EditorTheme::InspiredGitHub,
    ];

    pub fn label(self) -> &'static str {
        match self {
            EditorTheme::GitHubDark => "GitHub Dark",
            EditorTheme::OceanDark => "Ocean Dark",
            EditorTheme::EightiesDark => "Eighties Dark",
            EditorTheme::MochaDark => "Mocha Dark",
            EditorTheme::SolarizedDark => "Solarized Dark",
            EditorTheme::OceanLight => "Ocean Light",
            EditorTheme::SolarizedLight => "Solarized Light",
            EditorTheme::InspiredGitHub => "InspiredGitHub",
        }
    }

    pub fn syntect_name(self) -> &'static str {
        match self {
            EditorTheme::GitHubDark => "GitHub Dark",
            EditorTheme::OceanDark => "base16-ocean.dark",
            EditorTheme::EightiesDark => "base16-eighties.dark",
            EditorTheme::MochaDark => "base16-mocha.dark",
            EditorTheme::SolarizedDark => "Solarized (dark)",
            EditorTheme::OceanLight => "base16-ocean.light",
            EditorTheme::SolarizedLight => "Solarized (light)",
            EditorTheme::InspiredGitHub => "InspiredGitHub",
        }
    }

    pub fn is_dark(self) -> bool {
        matches!(
            self,
            EditorTheme::GitHubDark
                | EditorTheme::OceanDark
                | EditorTheme::EightiesDark
                | EditorTheme::MochaDark
                | EditorTheme::SolarizedDark
        )
    }
}

struct SyntaxResources {
    syntax_set: SyntaxSet,
    theme_set: ThemeSet,
}

#[derive(PartialEq, Eq)]
struct CacheKey {
    code_hash: u64,
    code_len: usize,
    extension: Option<String>,
    theme: EditorTheme,
}

struct HighlightCache {
    entries: Vec<(CacheKey, LayoutJob)>,
}

impl HighlightCache {
    const MAX_ENTRIES: usize = 16;

    fn new() -> Self {
        Self {
            entries: Vec::with_capacity(Self::MAX_ENTRIES),
        }
    }

    fn get(&self, key: &CacheKey) -> Option<LayoutJob> {
        self.entries
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, job)| job.clone())
    }

    fn insert(&mut self, key: CacheKey, job: LayoutJob) {
        if let Some(pos) = self.entries.iter().position(|(k, _)| k == &key) {
            self.entries.remove(pos);
        } else if self.entries.len() >= Self::MAX_ENTRIES {
            self.entries.remove(0);
        }
        self.entries.push((key, job));
    }
}

fn resources() -> &'static SyntaxResources {
    static RESOURCES: OnceLock<SyntaxResources> = OnceLock::new();
    RESOURCES.get_or_init(|| {
        let syntax_set = SyntaxSet::load_defaults_newlines();
        let mut theme_set = ThemeSet::load_defaults();

        let mut reader = Cursor::new(GITHUB_DARK_TMTHEME.as_bytes());
        if let Ok(github_dark) = ThemeSet::load_from_reader(&mut reader) {
            theme_set
                .themes
                .insert("GitHub Dark".to_string(), github_dark);
        }

        SyntaxResources {
            syntax_set,
            theme_set,
        }
    })
}

fn active_theme_lock() -> &'static Mutex<EditorTheme> {
    static ACTIVE_THEME: OnceLock<Mutex<EditorTheme>> = OnceLock::new();
    ACTIVE_THEME.get_or_init(|| Mutex::new(EditorTheme::default()))
}

fn cache() -> &'static Mutex<HighlightCache> {
    static CACHE: OnceLock<Mutex<HighlightCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HighlightCache::new()))
}

pub fn set_theme(theme: EditorTheme) {
    if let Ok(mut guard) = active_theme_lock().lock() {
        *guard = theme;
    }
}

pub fn current_theme() -> EditorTheme {
    active_theme_lock()
        .lock()
        .map(|guard| *guard)
        .unwrap_or_default()
}

fn hash_code(code: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    code.hash(&mut hasher);
    hasher.finish()
}

pub fn highlight(code: &str, extension: Option<&str>) -> LayoutJob {
    highlight_with_theme(code, extension, current_theme())
}

pub fn highlight_with_theme(
    code: &str,
    extension: Option<&str>,
    theme: EditorTheme,
) -> LayoutJob {
    if code.is_empty() {
        return LayoutJob::default();
    }

    let key = CacheKey {
        code_hash: hash_code(code),
        code_len: code.len(),
        extension: extension.map(|ext| ext.to_ascii_lowercase()),
        theme,
    };

    if let Ok(guard) = cache().lock() {
        if let Some(cached_job) = guard.get(&key) {
            return cached_job;
        }
    }

    let job = build_layout_job(code, key.extension.as_deref(), theme);

    if let Ok(mut guard) = cache().lock() {
        guard.insert(key, job.clone());
    }

    job
}

fn build_layout_job(code: &str, extension: Option<&str>, editor_theme: EditorTheme) -> LayoutJob {
    let res = resources();
    let syntax = extension
        .and_then(|ext| {
            res.syntax_set
                .find_syntax_by_extension(ext)
                .or_else(|| res.syntax_set.find_syntax_by_token(ext))
        })
        .unwrap_or_else(|| res.syntax_set.find_syntax_plain_text());

    let theme = res
        .theme_set
        .themes
        .get(editor_theme.syntect_name())
        .or_else(|| res.theme_set.themes.values().next())
        .expect("ThemeSet implicit nu conține nicio temă");

    let mut highlighter = HighlightLines::new(syntax, theme);
    let mut job = LayoutJob::default();
    let font_id = FontId::monospace(14.0);

    for line in LinesWithEndings::from(code) {
        let ranges: Vec<(Style, &str)> = highlighter
            .highlight_line(line, &res.syntax_set)
            .unwrap_or_else(|_| vec![(Style::default(), line)]);

        for (style, text) in ranges {
            let fg = style.foreground;
            let color = Color32::from_rgba_unmultiplied(fg.r, fg.g, fg.b, fg.a);

            let italics = style.font_style.contains(FontStyle::ITALIC);
            let underline = if style.font_style.contains(FontStyle::UNDERLINE) {
                Stroke::new(1.0_f32, color)
            } else {
                Stroke::NONE
            };

            job.append(
                text,
                0.0,
                TextFormat {
                    font_id: font_id.clone(),
                    color,
                    italics,
                    underline,
                    ..Default::default()
                },
            );
        }
    }

    job
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_rust_code_with_github_dark() {
        let sample = "pub fn main() {\n    let x = 42;\n}\n";
        let job = highlight_with_theme(sample, Some("rs"), EditorTheme::GitHubDark);

        assert_eq!(job.text, sample);
        assert!(job.sections.len() > 1);
    }

    #[test]
    fn different_themes_produce_different_colors() {
        let sample = "pub fn answer() -> i32 { 42 }";
        let dark_job = highlight_with_theme(sample, Some("rs"), EditorTheme::GitHubDark);
        let light_job = highlight_with_theme(sample, Some("rs"), EditorTheme::InspiredGitHub);

        assert_ne!(dark_job.sections, light_job.sections);
    }
}