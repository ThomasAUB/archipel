#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Integration tests for archipel-theme.

use archipel_theme::editor::{self, CONTRAST_PAIRS, pair_ratio};
use archipel_theme::registry::ThemeRegistry;
use archipel_theme::{
    Appearance, BaseColors, Color, ColorToken, Density, LANE_COUNT, Oklch, Theme, ThemeFile,
    ThemeSelection, ThemeWarning, WCAG_AA_LARGE_TEXT, WCAG_AA_NORMAL_TEXT, builtin, derive_colors,
    export_theme,
};

fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

// ---------------------------------------------------------------- color

#[test]
fn oklch_reference_values() {
    // Reference values from Ottosson's OKLab / CSS Color 4.
    let cases = [
        ("#ffffff", 1.0, 0.0, None),
        ("#000000", 0.0, 0.0, None),
        ("#ff0000", 0.627_955, 0.257_683, Some(29.2339)),
        ("#00ff00", 0.866_440, 0.294_827, Some(142.4953)),
        ("#0000ff", 0.452_014, 0.313_214, Some(264.0520)),
    ];
    for (hex, l, c, h) in cases {
        let lch = Color::from_hex(hex).unwrap().to_oklch();
        assert!(close(lch.l, l, 1e-4), "{hex} L {}", lch.l);
        assert!(close(lch.c, c, 1e-4), "{hex} C {}", lch.c);
        if let Some(h) = h {
            assert!(close(lch.h, h, 0.05), "{hex} h {}", lch.h);
        }
    }
}

#[test]
fn oklch_round_trip_is_lossless_for_srgb_colors() {
    for r in (0..=255).step_by(17) {
        for g in (0..=255).step_by(51) {
            for b in (0..=255).step_by(15) {
                let c = Color::rgba(r, g, b, 200);
                assert_eq!(c.to_oklch().to_color(), c);
                assert_eq!(c.to_oklab().to_color(), c);
            }
        }
    }
}

#[test]
fn out_of_gamut_oklch_is_clamped_preserving_lightness_and_hue() {
    let wild = Oklch::new(0.7, 0.4, 145.0);
    let c = wild.to_color();
    let back = c.to_oklch();
    assert!(close(back.l, 0.7, 0.01), "L {}", back.l);
    assert!(close(back.h, 145.0, 2.0), "h {}", back.h);
    assert!(back.c < 0.4);
    assert_eq!(Oklch::new(1.5, 0.0, 0.0).to_color(), Color::WHITE);
    assert_eq!(Oklch::new(-1.0, 0.1, 0.0).to_color(), Color::BLACK);
}

#[test]
fn wcag_contrast_reference_values() {
    assert!(close(Color::BLACK.contrast_ratio(Color::WHITE), 21.0, 1e-9));
    assert!(close(Color::WHITE.contrast_ratio(Color::BLACK), 21.0, 1e-9));
    assert!(close(Color::WHITE.contrast_ratio(Color::WHITE), 1.0, 1e-12));
    // #777777 on white is the classic ~4.48:1 (just below AA).
    let gray = Color::rgb(0x77, 0x77, 0x77);
    assert!(close(gray.contrast_ratio(Color::WHITE), 4.48, 0.01));
    assert!(close(
        Color::rgb(255, 0, 0).relative_luminance(),
        0.2126,
        1e-9
    ));
}

// ---------------------------------------------------------------- derivation

fn assert_text_aa(theme_desc: &str, colors: &archipel_theme::ColorSet) {
    for (fg, bg) in [
        (ColorToken::FgDefault, ColorToken::BgApp),
        (ColorToken::FgDefault, ColorToken::BgPanel),
        (ColorToken::FgDefault, ColorToken::BgSelected),
        (ColorToken::FgMuted, ColorToken::BgApp),
        (ColorToken::FgMuted, ColorToken::BgPanel),
        (ColorToken::AccentFg, ColorToken::Accent),
    ] {
        let r = colors[fg].contrast_ratio(colors[bg]);
        assert!(r >= WCAG_AA_NORMAL_TEXT, "{theme_desc}: {fg}/{bg} = {r:.2}");
    }
}

#[test]
fn derivation_gives_aa_text_for_light_and_dark_even_with_poor_input() {
    let bases = [
        (
            "dark",
            BaseColors {
                background: Color::rgb(0x20, 0x20, 0x28),
                text: Color::rgb(0xdd, 0xdd, 0xdd),
                accent: Color::rgb(0x88, 0x55, 0xff),
            },
        ),
        (
            "light",
            BaseColors {
                background: Color::rgb(0xff, 0xff, 0xff),
                text: Color::rgb(0x30, 0x30, 0x30),
                accent: Color::rgb(0x00, 0x80, 0x60),
            },
        ),
        (
            "dark low-contrast text",
            BaseColors {
                background: Color::rgb(0x30, 0x30, 0x30),
                text: Color::rgb(0x50, 0x50, 0x50),
                accent: Color::rgb(0xff, 0x80, 0x00),
            },
        ),
        (
            "light low-contrast text",
            BaseColors {
                background: Color::rgb(0xf0, 0xe8, 0xd8),
                text: Color::rgb(0xc0, 0xb8, 0xa8),
                accent: Color::rgb(0xff, 0xee, 0x00),
            },
        ),
    ];
    for (desc, base) in bases {
        let colors = derive_colors(&base);
        assert_text_aa(desc, &colors);
        assert!(
            editor::contrast_warnings(&colors)
                .iter()
                .all(|w| matches!((w.fg, w.bg), (ColorToken::Accent, ColorToken::BgApp))),
            "{desc}: {:?}",
            editor::contrast_warnings(&colors)
        );
    }
}

#[test]
fn derivation_is_deterministic_and_appearance_aware() {
    let dark = builtin::dark();
    let again = derive_colors(dark.base());
    assert_eq!(&again, dark.colors());
    assert_eq!(dark.appearance(), Appearance::Dark);
    assert_eq!(builtin::light().appearance(), Appearance::Light);
    // Hover is lighter than the app background on dark, darker on light.
    let l = |t: &Theme, tok| t.color(tok).to_oklch().l;
    assert!(l(&dark, ColorToken::BgHover) > l(&dark, ColorToken::BgApp));
    let light = builtin::light();
    assert!(l(&light, ColorToken::BgHover) < l(&light, ColorToken::BgApp));
}

#[test]
fn lane_palette_is_distinguishable_and_readable() {
    for theme in builtin::all() {
        let c = theme.colors();
        let bg = c[ColorToken::BgApp];
        for i in 0..LANE_COUNT {
            let r = c.lane(i).contrast_ratio(bg);
            assert!(r >= WCAG_AA_LARGE_TEXT, "{} lane {i}: {r:.2}", theme.id);
            for j in (i + 1)..LANE_COUNT {
                let d = c.lane(i).delta_e(c.lane(j));
                assert!(d >= 0.05, "{} lanes {i}/{j} too close: {d:.3}", theme.id);
            }
        }
        // Adjacent lanes are clearly different.
        for i in 0..LANE_COUNT {
            let d = c.lane(i).delta_e(c.lane(i + 1));
            assert!(d >= 0.15, "{} adjacent lanes {i}: {d:.3}", theme.id);
        }
    }
}

/// Every theme re-derived with accents all around the wheel, muted to vivid.
fn accent_sweep() -> impl Iterator<Item = (Theme, f64)> {
    builtin::all().into_iter().flat_map(|theme| {
        (0..120).flat_map(move |step| {
            let hue = f64::from(step) * 3.0;
            let theme = theme.clone();
            [(0.68, 0.05), (0.68, 0.12), (0.6, 0.2), (0.85, 0.15)]
                .into_iter()
                .map(move |(l, c)| {
                    let accent = Oklch::new(l, c, hue).to_color();
                    let themed = archipel_theme::with_project_accent(&theme, accent);
                    (themed, accent.to_oklch().h)
                })
        })
    })
}

#[test]
fn lane_palette_follows_any_accent_and_stays_distinguishable() {
    for (theme, accent_hue) in accent_sweep() {
        let c = theme.colors();
        let bg = c[ColorToken::BgApp];
        // The main lane carries the accent's hue (outside the muddy band).
        if !(95.0..135.0).contains(&accent_hue) {
            let h = c.lane(0).to_oklch().h;
            let d = (h - accent_hue + 180.0).rem_euclid(360.0) - 180.0;
            assert!(
                d.abs() < 6.0,
                "{} lane 0 hue {h:.0} vs {accent_hue:.0}",
                theme.id
            );
        }
        for i in 0..LANE_COUNT {
            assert!(c.lane(i).contrast_ratio(bg) >= WCAG_AA_LARGE_TEXT);
            assert!(
                c.lane(i).delta_e(c.lane(i + 1)) >= 0.15,
                "{} adjacent {i}",
                theme.id
            );
            for j in (i + 1)..LANE_COUNT {
                let d = c.lane(i).delta_e(c.lane(j));
                assert!(
                    d >= 0.05,
                    "{} accent {accent_hue:.0}: lanes {i}/{j}",
                    theme.id
                );
            }
        }
    }
}

#[test]
fn status_colors_lean_towards_the_accent_but_keep_their_meaning() {
    let hue = |t: &Theme, tok| t.color(tok).to_oklch().h;
    for (theme, _) in accent_sweep() {
        // Green stays green, red stays red, yellow stays yellow.
        for (tok, lo, hi) in [
            (ColorToken::StatusSuccess, 135.0, 165.0),
            (ColorToken::DiffAddFg, 135.0, 165.0),
            (ColorToken::StatusWarning, 45.0, 75.0),
            (ColorToken::StatusDirty, 65.0, 95.0),
        ] {
            let h = hue(&theme, tok);
            assert!((lo..=hi).contains(&h), "{} {tok:?}: {h:.0}", theme.id);
        }
        for tok in [ColorToken::StatusError, ColorToken::DiffDelFg] {
            let h = (hue(&theme, tok) + 180.0).rem_euclid(360.0) - 180.0;
            assert!((10.0..=40.0).contains(&h), "{} {tok:?}: {h:.0}", theme.id);
        }
    }
    // A warm accent warms the green, a cool one cools it.
    let dark = builtin::dark();
    let with = |h| archipel_theme::with_project_accent(&dark, Oklch::new(0.7, 0.12, h).to_color());
    let success = |t: &Theme| hue(t, ColorToken::StatusSuccess);
    assert!(success(&with(60.0)) < success(&with(230.0)));
    // Syntax colors follow the same way.
    let string = |t: &Theme| hue(t, ColorToken::SyntaxString);
    assert!(string(&with(60.0)) < string(&with(230.0)));
}

// ---------------------------------------------------------------- built-ins

#[test]
fn every_builtin_passes_aa_on_all_checked_pairs() {
    for theme in builtin::all() {
        let c = theme.colors();
        assert!(c[ColorToken::FgDefault].contrast_ratio(c[ColorToken::BgApp]) >= 4.5);
        assert!(c[ColorToken::FgMuted].contrast_ratio(c[ColorToken::BgPanel]) >= 4.5);
        let warnings = editor::contrast_warnings(c);
        assert!(warnings.is_empty(), "{}: {warnings:?}", theme.id);
    }
    let hc = builtin::high_contrast();
    for p in CONTRAST_PAIRS
        .iter()
        .filter(|p| matches!(p.fg, ColorToken::FgDefault | ColorToken::FgMuted))
    {
        assert!(pair_ratio(hc.colors(), p) >= 7.0, "{p:?}");
    }
}

#[test]
fn follow_os_selects_configured_themes() {
    let reg = ThemeRegistry::new();
    let sel = ThemeSelection::default();
    assert_eq!(reg.select(&sel, Appearance::Light).id, "light");
    assert_eq!(reg.select(&sel, Appearance::Dark).id, "dark");
    let sel = ThemeSelection::FollowOs {
        light: "light".into(),
        dark: "high-contrast".into(),
    };
    assert_eq!(reg.select(&sel, Appearance::Dark).id, "high-contrast");
    let missing = ThemeSelection::Fixed { id: "nope".into() };
    assert_eq!(reg.select(&missing, Appearance::Light).id, "dark");
}

// ---------------------------------------------------------------- files

#[test]
fn file_parsing_with_all_sections() {
    let text = r##"
name = "Midnight"
extends = "dark"
density = "compact"
radius = 3
animation = "off"

[base]
accent = "#ff5577"

[typography]
ui_font = "IBM Plex Sans"
size = 14.5

[tokens]
"bg.panel" = "#0a0b0c"
diff.add.bg = "#002200"
"##;
    let (file, warnings) = ThemeFile::parse(text).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    let theme = file.apply("midnight", &builtin::dark());
    assert_eq!(theme.name, "Midnight");
    assert_eq!(theme.density, Density::Compact);
    assert!((theme.radius - 3.0).abs() < f32::EPSILON);
    assert!(!theme.animation.is_enabled());
    assert_eq!(theme.typography.ui_font, "IBM Plex Sans");
    assert_eq!(
        theme.typography.mono_font,
        builtin::dark().typography.mono_font
    );
    assert_eq!(
        theme.color(ColorToken::Accent),
        Color::rgb(0xff, 0x55, 0x77)
    );
    assert_eq!(
        theme.color(ColorToken::BgPanel),
        Color::rgb(0x0a, 0x0b, 0x0c)
    );
    assert_eq!(theme.color(ColorToken::DiffAddBg), Color::rgb(0, 0x22, 0));
    // Background unchanged from parent.
    assert_eq!(
        theme.color(ColorToken::BgApp),
        builtin::dark().color(ColorToken::BgApp)
    );
}

#[test]
fn invalid_values_warn_and_fall_back_to_parent() {
    let text = r##"
name = 42
density = "cozy"
radius = 999
mystery = true

[base]
accent = "red"
glow = "#ffffff"

[typography]
size = "big"

[tokens]
"bg.app" = "#12345"
"fg.nope" = "#ffffff"
"fg.muted" = 7
"bg.panel" = "#010203"
"##;
    let (file, warnings) = ThemeFile::parse(text).unwrap();
    let parent = builtin::light();
    let theme = file.apply("broken-ish", &parent);
    // Valid part applied, everything else from the parent.
    assert_eq!(theme.color(ColorToken::BgPanel), Color::rgb(1, 2, 3));
    assert_eq!(
        theme.color(ColorToken::BgApp),
        parent.color(ColorToken::BgApp)
    );
    assert_eq!(
        theme.color(ColorToken::FgMuted),
        parent.color(ColorToken::FgMuted)
    );
    assert_eq!(
        theme.color(ColorToken::Accent),
        parent.color(ColorToken::Accent)
    );
    assert_eq!(theme.name, "broken-ish");
    assert_eq!(theme.density, parent.density);
    assert!((theme.radius - parent.radius).abs() < f32::EPSILON);
    assert!((theme.typography.size - parent.typography.size).abs() < f32::EPSILON);

    let has = |pred: &dyn Fn(&ThemeWarning) -> bool| warnings.iter().any(pred);
    assert!(has(
        &|w| matches!(w, ThemeWarning::UnknownToken { key } if key == "fg.nope")
    ));
    assert!(has(
        &|w| matches!(w, ThemeWarning::UnknownKey { key } if key == "mystery")
    ));
    assert!(has(
        &|w| matches!(w, ThemeWarning::UnknownKey { key } if key == "base.glow")
    ));
    for key in [
        "name",
        "density",
        "radius",
        "base.accent",
        "typography.size",
        "tokens.bg.app",
        "tokens.fg.muted",
    ] {
        assert!(
            has(&|w| matches!(w, ThemeWarning::InvalidValue { key: k, .. } if k == key)),
            "missing warning for {key}: {warnings:?}"
        );
    }
    assert_eq!(warnings.len(), 10, "{warnings:?}");
}

#[test]
fn syntax_error_is_an_error_for_strict_parse() {
    assert!(ThemeFile::parse("name = \"unterminated").is_err());
}

#[test]
fn base_override_rederives_but_keeps_parent_overrides() {
    let hc = builtin::high_contrast();
    let (file, _) =
        ThemeFile::parse("extends = \"high-contrast\"\n[base]\naccent = \"#00ccff\"\n").unwrap();
    let t = file.apply("hc-blue", &hc);
    assert_eq!(t.color(ColorToken::Accent), Color::rgb(0, 0xcc, 0xff));
    // Parent's explicit overrides survive.
    assert_eq!(t.color(ColorToken::FgMuted), hc.color(ColorToken::FgMuted));
    assert_ne!(
        t.color(ColorToken::BgSelected),
        hc.color(ColorToken::BgSelected)
    );
}

#[test]
fn export_is_minimal_and_import_round_trips() {
    let parent = builtin::dark();
    let mut theme = editor::duplicate(&parent, "ocean", "Ocean");
    // Untouched duplicate exports only name + extends.
    let minimal = export_theme(&theme, &parent);
    let body: Vec<_> = minimal
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .collect();
    assert_eq!(body, ["name = \"Ocean\"", "extends = \"dark\""]);

    theme.set_base(BaseColors {
        background: Color::rgb(0x0d, 0x1b, 0x2a),
        ..*parent.base()
    });
    theme.set_token(ColorToken::GraphLane3, Color::rgb(0xaa, 0xbb, 0xcc));
    theme.set_token(ColorToken::DiffDelWordBg, Color::rgba(0xff, 0, 0, 0x40));
    theme.typography.mono_font = "Fira \"Code\"".into();
    theme.typography.line_height = 1.6;
    theme.density = Density::Spacious;
    theme.radius = 2.5;

    let text = export_theme(&theme, &parent);
    assert!(text.contains("[base]") && text.contains("background = \"#0d1b2a\""));
    assert!(!text.contains("accent ="), "{text}");
    assert!(text.contains("\"graph.lane.3\" = \"#aabbcc\""));
    assert!(text.contains("\"diff.del.word.bg\" = \"#ff000040\""));
    assert_eq!(text.matches(" = \"#").count(), 3, "{text}");

    let (file, warnings) = ThemeFile::parse(&text).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    let imported = file.apply("ocean", &parent);
    assert_eq!(imported.colors(), theme.colors());
    assert_eq!(imported.name, theme.name);
    assert_eq!(imported.typography, theme.typography);
    assert_eq!(imported.density, theme.density);
    assert!((imported.radius - theme.radius).abs() < f32::EPSILON);
    assert_eq!(imported.animation, theme.animation);
    // Exporting the import gives the same text.
    assert_eq!(export_theme(&imported, &parent), text);
}

// ---------------------------------------------------------------- registry

fn write(dir: &std::path::Path, name: &str, text: &str) {
    std::fs::write(dir.join(name), text).unwrap();
}

#[test]
fn registry_loads_directory_with_chains_cycles_and_broken_files() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(
        d,
        "base-blue.toml",
        "name = \"Base blue\"\nextends = \"light\"\n[base]\naccent = \"#0044cc\"\n",
    );
    write(
        d,
        "child.toml",
        "name = \"Child\"\nextends = \"base-blue\"\n[tokens]\n\"bg.panel\" = \"#eeeeee\"\n",
    );
    write(d, "grandchild.toml", "extends = \"child\"\nradius = 0.0\n");
    write(d, "loop-a.toml", "extends = \"loop-b\"\n");
    write(d, "loop-b.toml", "extends = \"loop-a\"\n");
    write(d, "orphan.toml", "extends = \"does-not-exist\"\n");
    write(d, "broken.toml", "name = \"Broken\nthis is not toml");
    write(d, "dark.toml", "name = \"Impostor\"\n");
    write(d, "notes.txt", "ignored");

    let mut reg = ThemeRegistry::new();
    reg.load_dir(d).unwrap();

    let ids: Vec<_> = reg.ids().collect();
    assert_eq!(
        ids,
        [
            "light",
            "dark",
            "high-contrast",
            "base-blue",
            "broken",
            "child",
            "grandchild",
            "loop-a",
            "loop-b",
            "orphan"
        ]
    );

    // Chain: grandchild -> child -> base-blue -> light.
    let gc = reg.get("grandchild").unwrap();
    assert_eq!(gc.extends.as_deref(), Some("child"));
    assert_eq!(gc.appearance(), Appearance::Light);
    assert_eq!(gc.color(ColorToken::Accent), Color::rgb(0, 0x44, 0xcc));
    assert_eq!(gc.color(ColorToken::BgPanel), Color::rgb(0xee, 0xee, 0xee));
    assert!(gc.radius.abs() < f32::EPSILON);
    assert_eq!(gc.name, "grandchild");
    assert_eq!(reg.parent_of("grandchild").unwrap().id, "child");
    assert!(reg.warnings("grandchild").is_empty());

    // Cycle: detected, falls back to the default built-in, still usable.
    let cycle_warnings: Vec<_> = ["loop-a", "loop-b"]
        .iter()
        .flat_map(|id| reg.warnings(id))
        .filter(|w| matches!(w, ThemeWarning::ExtendsCycle { .. }))
        .collect();
    assert_eq!(cycle_warnings.len(), 1, "{cycle_warnings:?}");
    assert!(reg.get("loop-a").is_some() && reg.get("loop-b").is_some());

    // Unknown parent.
    assert!(matches!(
        reg.warnings("orphan"),
        [ThemeWarning::UnknownParent { parent, .. }] if parent == "does-not-exist"
    ));
    assert_eq!(
        reg.get("orphan").unwrap().colors(),
        builtin::dark().colors()
    );

    // Broken file: usable fallback theme plus a warning.
    let broken = reg.get("broken").unwrap();
    assert_eq!(broken.colors(), builtin::dark().colors());
    assert!(matches!(
        reg.warnings("broken"),
        [ThemeWarning::Unparsable { .. }]
    ));

    // Built-in ids cannot be shadowed.
    assert_eq!(reg.get("dark").unwrap().name, "Dark");
    assert!(matches!(
        reg.warnings("dark"),
        [ThemeWarning::ReservedId { .. }]
    ));
}

#[test]
fn registry_reload_single_file_updates_dependents() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(
        d,
        "parent.toml",
        "extends = \"dark\"\n[base]\naccent = \"#ff0000\"\n",
    );
    write(d, "kid.toml", "extends = \"parent\"\n");
    let mut reg = ThemeRegistry::new();
    reg.load_dir(d).unwrap();
    assert_eq!(
        reg.get("kid").unwrap().color(ColorToken::Accent),
        Color::rgb(255, 0, 0)
    );

    write(
        d,
        "parent.toml",
        "extends = \"dark\"\n[base]\naccent = \"#00ff00\"\n",
    );
    assert_eq!(
        reg.reload_file(&d.join("parent.toml")).as_deref(),
        Some("parent")
    );
    assert_eq!(
        reg.get("kid").unwrap().color(ColorToken::Accent),
        Color::rgb(0, 255, 0)
    );

    // Breaking the parent keeps the child usable.
    write(d, "parent.toml", "[[[");
    reg.reload_file(&d.join("parent.toml"));
    assert!(reg.get("kid").is_some());
    assert!(!reg.warnings("parent").is_empty());

    // Deleting it: the child falls back with a warning.
    std::fs::remove_file(d.join("parent.toml")).unwrap();
    reg.reload_file(&d.join("parent.toml"));
    assert!(reg.get("parent").is_none());
    assert!(matches!(
        reg.warnings("kid"),
        [ThemeWarning::UnknownParent { .. }]
    ));
    assert_eq!(reg.reload_file(&d.join("readme.md")), None);
}

#[test]
fn registry_missing_dir_and_import() {
    let dir = tempfile::tempdir().unwrap();
    let mut reg = ThemeRegistry::new();
    reg.load_dir(&dir.path().join("themes")).unwrap();
    assert_eq!(reg.themes().count(), 3);

    let exported = export_theme(
        &editor::duplicate(&builtin::light(), "x", "Shared"),
        &builtin::light(),
    );
    let t = reg.import_str("shared", &exported).unwrap();
    assert_eq!(t.name, "Shared");
    assert_eq!(t.colors(), builtin::light().colors());
    assert!(reg.import_str("light", &exported).is_err());
    assert!(reg.import_str("bad", "= nope").is_err());
    // A later load_dir keeps imported (in-memory) themes.
    reg.load_dir(dir.path()).unwrap();
    assert!(reg.get("shared").is_some());
}
