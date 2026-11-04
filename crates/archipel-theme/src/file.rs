//! TOML theme files: lenient parsing with warnings, resolution against a
//! parent theme, and minimal export (only what differs from the parent).
//!
//! Format:
//!
//! ```toml
//! name = "Midnight"
//! extends = "dark"          # built-in or another user theme id
//! density = "compact"       # compact | comfortable | spacious
//! radius = 4.0              # corner radius, px
//! animation = "off"         # off | slow | normal | fast
//!
//! [base]                    # derive every token from these
//! background = "#101418"
//! text = "#e6e6e6"
//! accent = "#7aa2f7"
//!
//! [typography]
//! ui_font = "Inter"
//! mono_font = "JetBrains Mono"
//! size = 13.0
//! line_height = 1.45
//!
//! [tokens]                  # individual overrides by token id
//! "bg.panel" = "#0c0f12"
//! ```

use std::collections::BTreeMap;
use std::fmt::Write as _;

use toml::{Table, Value};

use crate::color::Color;
use crate::derive::BaseColors;
use crate::error::{ThemeError, ThemeWarning};
use crate::settings::{
    AnimationSpeed, Density, MAX_FONT_SIZE, MAX_LINE_HEIGHT, MAX_RADIUS, MIN_FONT_SIZE,
    MIN_LINE_HEIGHT,
};
use crate::theme::Theme;
use crate::token::ColorToken;

/// Optional base color overrides from a `[base]` table.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BaseOverrides {
    /// Base background.
    pub background: Option<Color>,
    /// Base text color.
    pub text: Option<Color>,
    /// Accent color.
    pub accent: Option<Color>,
}

impl BaseOverrides {
    /// Whether no base color is set.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.background.is_none() && self.text.is_none() && self.accent.is_none()
    }
}

/// Optional typography overrides from a `[typography]` table.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TypographyOverrides {
    /// UI font family.
    pub ui_font: Option<String>,
    /// Monospace font family.
    pub mono_font: Option<String>,
    /// Base font size (px).
    pub size: Option<f32>,
    /// Line height (multiple of the size).
    pub line_height: Option<f32>,
}

impl TypographyOverrides {
    /// Whether nothing is set.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.ui_font.is_none()
            && self.mono_font.is_none()
            && self.size.is_none()
            && self.line_height.is_none()
    }
}

/// A parsed theme file: only the values it sets. Everything else comes from
/// the parent theme when [applied](ThemeFile::apply).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ThemeFile {
    /// Display name (defaults to the theme id).
    pub name: Option<String>,
    /// Parent theme id (defaults to [`crate::builtin::DEFAULT_ID`]).
    pub extends: Option<String>,
    /// Base colors; when any is set, the theme is re-derived from them.
    pub base: BaseOverrides,
    /// Individual token overrides.
    pub tokens: BTreeMap<ColorToken, Color>,
    /// Typography overrides.
    pub typography: TypographyOverrides,
    /// Density.
    pub density: Option<Density>,
    /// Corner radius (px).
    pub radius: Option<f32>,
    /// Animation speed.
    pub animation: Option<AnimationSpeed>,
}

impl ThemeFile {
    /// Parses theme file text. Only invalid TOML is an error; unknown keys and
    /// invalid values are dropped (so the parent value applies) and reported
    /// as warnings.
    pub fn parse(text: &str) -> Result<(Self, Vec<ThemeWarning>), ThemeError> {
        let table: Table = toml::from_str(text).map_err(|e| ThemeError::Parse {
            message: e.to_string().trim().to_owned(),
        })?;
        let mut p = Parser::default();
        for (key, value) in &table {
            p.top_level(key, value);
        }
        Ok((p.file, p.warnings))
    }

    /// Resolves this file on top of `parent`, producing a complete theme
    /// with id `id`.
    #[must_use]
    pub fn apply(&self, id: &str, parent: &Theme) -> Theme {
        let mut theme = parent.clone();
        id.clone_into(&mut theme.id);
        theme.name = self.name.clone().unwrap_or_else(|| id.to_owned());
        theme.extends = Some(parent.id.clone());

        let pb = parent.base();
        let base = BaseColors {
            background: self.base.background.unwrap_or(pb.background),
            text: self.base.text.unwrap_or(pb.text),
            accent: self.base.accent.unwrap_or(pb.accent),
        };
        let mut overrides = parent.overrides().clone();
        overrides.extend(self.tokens.iter().map(|(&t, &c)| (t, c)));
        theme.set_base(base);
        theme.set_overrides(overrides);

        let t = &self.typography;
        if let Some(f) = &t.ui_font {
            theme.typography.ui_font.clone_from(f);
        }
        if let Some(f) = &t.mono_font {
            theme.typography.mono_font.clone_from(f);
        }
        if let Some(s) = t.size {
            theme.typography.size = s;
        }
        if let Some(l) = t.line_height {
            theme.typography.line_height = l;
        }
        if let Some(d) = self.density {
            theme.density = d;
        }
        if let Some(r) = self.radius {
            theme.radius = r;
        }
        if let Some(a) = self.animation {
            theme.animation = a;
        }
        theme
    }

    /// The minimal file that turns `parent` into `theme`: name, `extends`,
    /// and only the values that differ.
    #[must_use]
    pub fn diff(theme: &Theme, parent: &Theme) -> Self {
        let (tb, pb) = (theme.base(), parent.base());
        let changed = |a: Color, b: Color| (a != b).then_some(a);
        let base = BaseOverrides {
            background: changed(tb.background, pb.background),
            text: changed(tb.text, pb.text),
            accent: changed(tb.accent, pb.accent),
        };
        // What the file would produce without [tokens]: the theme's base with
        // the parent's overrides. Tokens that differ from it are written.
        let reference = Theme::colors_with(tb, parent.overrides());
        let tokens = theme
            .colors()
            .iter()
            .filter(|&(t, c)| reference.get(t) != c)
            .collect();

        let (tt, pt) = (&theme.typography, &parent.typography);
        let typography = TypographyOverrides {
            ui_font: (tt.ui_font != pt.ui_font).then(|| tt.ui_font.clone()),
            mono_font: (tt.mono_font != pt.mono_font).then(|| tt.mono_font.clone()),
            size: f32_changed(tt.size, pt.size),
            line_height: f32_changed(tt.line_height, pt.line_height),
        };
        Self {
            name: Some(theme.name.clone()),
            extends: Some(parent.id.clone()),
            base,
            tokens,
            typography,
            density: (theme.density != parent.density).then_some(theme.density),
            radius: f32_changed(theme.radius, parent.radius),
            animation: (theme.animation != parent.animation).then_some(theme.animation),
        }
    }

    /// Serializes to readable TOML (stable key order: settings, `[base]`,
    /// `[typography]`, `[tokens]` in token display order).
    #[must_use]
    pub fn to_toml(&self) -> String {
        let mut out = String::new();
        let line = |out: &mut String, key: &str, value: &str| {
            // Writing to a String cannot fail.
            let _ = writeln!(out, "{key} = {value}");
        };
        if let Some(n) = &self.name {
            line(&mut out, "name", &quote(n));
        }
        if let Some(e) = &self.extends {
            line(&mut out, "extends", &quote(e));
        }
        if let Some(d) = self.density {
            line(&mut out, "density", &quote(d.id()));
        }
        if let Some(r) = self.radius {
            line(&mut out, "radius", &fmt_f32(r));
        }
        if let Some(a) = self.animation {
            line(&mut out, "animation", &quote(a.id()));
        }
        if !self.base.is_empty() {
            out.push_str("\n[base]\n");
            for (k, v) in [
                ("background", self.base.background),
                ("text", self.base.text),
                ("accent", self.base.accent),
            ] {
                if let Some(c) = v {
                    line(&mut out, k, &quote(&c.to_hex()));
                }
            }
        }
        let t = &self.typography;
        if !t.is_empty() {
            out.push_str("\n[typography]\n");
            if let Some(f) = &t.ui_font {
                line(&mut out, "ui_font", &quote(f));
            }
            if let Some(f) = &t.mono_font {
                line(&mut out, "mono_font", &quote(f));
            }
            if let Some(s) = t.size {
                line(&mut out, "size", &fmt_f32(s));
            }
            if let Some(l) = t.line_height {
                line(&mut out, "line_height", &fmt_f32(l));
            }
        }
        if !self.tokens.is_empty() {
            out.push_str("\n[tokens]\n");
            for (tok, c) in &self.tokens {
                line(&mut out, &quote(tok.id()), &quote(&c.to_hex()));
            }
        }
        out
    }
}

/// Exports `theme` as a minimal, shareable TOML file relative to `parent`.
#[must_use]
pub fn export_theme(theme: &Theme, parent: &Theme) -> String {
    format!(
        "# Archipel theme. Only values that differ from \"{}\" are listed.\n{}",
        parent.id,
        ThemeFile::diff(theme, parent).to_toml()
    )
}

fn quote(s: &str) -> String {
    Value::String(s.to_owned()).to_string()
}

fn fmt_f32(v: f32) -> String {
    let s = v.to_string();
    if s.contains('.') { s } else { format!("{s}.0") }
}

fn f32_changed(a: f32, b: f32) -> Option<f32> {
    (a.to_bits() != b.to_bits()).then_some(a)
}

/// Narrows a range-checked `f64` to `f32`.
#[allow(
    clippy::cast_possible_truncation,
    reason = "callers range-check to small UI values; f32 precision is what the UI uses"
)]
fn to_f32(v: f64) -> f32 {
    v as f32
}

fn as_number(v: &Value) -> Option<f64> {
    match v {
        Value::Float(f) if f.is_finite() => Some(*f),
        Value::Integer(i) => i32::try_from(*i).ok().map(f64::from),
        _ => None,
    }
}

#[derive(Default)]
struct Parser {
    file: ThemeFile,
    warnings: Vec<ThemeWarning>,
}

impl Parser {
    fn invalid(&mut self, key: &str, reason: impl Into<String>) {
        self.warnings.push(ThemeWarning::InvalidValue {
            key: key.to_owned(),
            reason: reason.into(),
        });
    }

    fn string(&mut self, key: &str, v: &Value) -> Option<String> {
        match v {
            Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_owned()),
            Value::String(_) => {
                self.invalid(key, "empty string");
                None
            }
            _ => {
                self.invalid(key, format!("expected a string, found {}", v.type_str()));
                None
            }
        }
    }

    fn color(&mut self, key: &str, v: &Value) -> Option<Color> {
        let Value::String(s) = v else {
            self.invalid(
                key,
                format!("expected a color string, found {}", v.type_str()),
            );
            return None;
        };
        match Color::from_hex(s) {
            Ok(c) => Some(c),
            Err(e) => {
                self.invalid(key, format!("`{s}`: {e}"));
                None
            }
        }
    }

    fn number(&mut self, key: &str, v: &Value, min: f32, max: f32) -> Option<f32> {
        let Some(n) = as_number(v) else {
            self.invalid(key, format!("expected a number, found {}", v.type_str()));
            return None;
        };
        if (f64::from(min)..=f64::from(max)).contains(&n) {
            Some(to_f32(n))
        } else {
            self.invalid(key, format!("{n} is outside {min}..={max}"));
            None
        }
    }

    fn table<'a>(&mut self, key: &str, v: &'a Value) -> Option<&'a Table> {
        if let Value::Table(t) = v {
            Some(t)
        } else {
            self.invalid(key, format!("expected a table, found {}", v.type_str()));
            None
        }
    }

    fn top_level(&mut self, key: &str, v: &Value) {
        match key {
            "name" => self.file.name = self.string(key, v),
            "extends" => self.file.extends = self.string(key, v),
            "density" => {
                let parsed = self.string(key, v).and_then(|s| {
                    let d = Density::from_id(&s);
                    if d.is_none() {
                        self.invalid(
                            key,
                            format!("`{s}` is not compact, comfortable or spacious"),
                        );
                    }
                    d
                });
                self.file.density = parsed;
            }
            "animation" => {
                let parsed = self.string(key, v).and_then(|s| {
                    let a = AnimationSpeed::from_id(&s);
                    if a.is_none() {
                        self.invalid(key, format!("`{s}` is not off, slow, normal or fast"));
                    }
                    a
                });
                self.file.animation = parsed;
            }
            "radius" => self.file.radius = self.number(key, v, 0.0, MAX_RADIUS),
            "base" => {
                if let Some(t) = self.table(key, v) {
                    for (k, v) in t {
                        let path = format!("base.{k}");
                        let c = match k.as_str() {
                            "background" => &mut self.file.base.background,
                            "text" => &mut self.file.base.text,
                            "accent" => &mut self.file.base.accent,
                            _ => {
                                self.warnings.push(ThemeWarning::UnknownKey { key: path });
                                continue;
                            }
                        };
                        let parsed = Self::color_static(&path, v, &mut self.warnings);
                        *c = parsed;
                    }
                }
            }
            "typography" => {
                if let Some(t) = self.table(key, v) {
                    for (k, v) in t {
                        let path = format!("typography.{k}");
                        match k.as_str() {
                            "ui_font" => self.file.typography.ui_font = self.string(&path, v),
                            "mono_font" => self.file.typography.mono_font = self.string(&path, v),
                            "size" => {
                                self.file.typography.size =
                                    self.number(&path, v, MIN_FONT_SIZE, MAX_FONT_SIZE);
                            }
                            "line_height" => {
                                self.file.typography.line_height =
                                    self.number(&path, v, MIN_LINE_HEIGHT, MAX_LINE_HEIGHT);
                            }
                            _ => self.warnings.push(ThemeWarning::UnknownKey { key: path }),
                        }
                    }
                }
            }
            "tokens" => {
                if let Some(t) = self.table(key, v) {
                    self.tokens("", t);
                }
            }
            _ => self.warnings.push(ThemeWarning::UnknownKey {
                key: key.to_owned(),
            }),
        }
    }

    fn color_static(key: &str, v: &Value, warnings: &mut Vec<ThemeWarning>) -> Option<Color> {
        let mut p = Self::default();
        let c = p.color(key, v);
        warnings.append(&mut p.warnings);
        c
    }

    /// Token keys may be quoted (`"bg.app" = …`) or written as dotted keys,
    /// which TOML turns into nested tables; both are flattened to ids.
    fn tokens(&mut self, prefix: &str, table: &Table) {
        for (k, v) in table {
            let id = if prefix.is_empty() {
                k.clone()
            } else {
                format!("{prefix}.{k}")
            };
            if let Value::Table(inner) = v {
                self.tokens(&id, inner);
                continue;
            }
            let Some(token) = ColorToken::from_id(&id) else {
                self.warnings.push(ThemeWarning::UnknownToken { key: id });
                continue;
            };
            if let Some(c) = self.color(&format!("tokens.{id}"), v) {
                self.file.tokens.insert(token, c);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_formatting_keeps_a_decimal_point() {
        assert_eq!(fmt_f32(13.0), "13.0");
        assert_eq!(fmt_f32(1.45), "1.45");
    }

    #[test]
    fn dotted_token_keys_are_flattened() {
        let (f, w) = ThemeFile::parse("[tokens]\nbg.panel = \"#010203\"\n").unwrap_or_default();
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(
            f.tokens.get(&ColorToken::BgPanel),
            Some(&Color::rgb(1, 2, 3))
        );
    }
}
