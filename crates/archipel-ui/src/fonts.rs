//! Fonts shipped inside the binary, so the built-in themes look the same on
//! every machine. Licenses are next to the files in `crates/archipel-ui/fonts`.

use std::borrow::Cow;
use std::collections::HashSet;

use gpui_kit::App;

/// A bundled font family and its files.
struct Family {
    /// Family name, as themes refer to it.
    name: &'static str,
    /// The family's font files.
    files: &'static [&'static [u8]],
}

/// Families used by the built-in themes (Quiet Cyber).
const BUNDLED: &[Family] = &[
    // SIL Open Font License 1.1 (fonts/Jura-OFL.txt). Jura has no italic.
    Family {
        name: "Jura",
        files: &[
            include_bytes!("../fonts/Jura-Regular.ttf"),
            include_bytes!("../fonts/Jura-SemiBold.ttf"),
            include_bytes!("../fonts/Jura-Bold.ttf"),
        ],
    },
    // MIT + Bitstream Vera license (fonts/Hack-LICENSE.md).
    Family {
        name: "Hack",
        files: &[
            include_bytes!("../fonts/Hack-Regular.ttf"),
            include_bytes!("../fonts/Hack-Bold.ttf"),
            include_bytes!("../fonts/Hack-Italic.ttf"),
            include_bytes!("../fonts/Hack-BoldItalic.ttf"),
        ],
    },
];

/// Registers the bundled families missing from `installed` (lowercase
/// family names) and adds them to it. A family the user has installed is
/// left alone: their version wins.
pub fn register_missing(installed: &mut HashSet<String>, cx: &App) {
    let missing: Vec<&Family> = BUNDLED
        .iter()
        .filter(|f| !installed.contains(&f.name.to_lowercase()))
        .collect();
    if missing.is_empty() {
        return;
    }
    let files = missing
        .iter()
        .flat_map(|f| f.files.iter().map(|b| Cow::Borrowed(*b)))
        .collect();
    match cx.text_system().add_fonts(files) {
        Ok(()) => installed.extend(missing.iter().map(|f| f.name.to_lowercase())),
        Err(e) => tracing::warn!(error = %e, "cannot register the bundled fonts"),
    }
}
