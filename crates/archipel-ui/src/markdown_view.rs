//! Markdown files, rendered: the file viewer shows a `.md` file as rich
//! text (gpui-kit's text view, GFM), its ```` ```mermaid ```` blocks as
//! diagrams drawn without a browser, its images read from the same commit
//! (never from the network), and its links followed in the viewer.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use archipel_theme::ColorToken as T;
use gpui_kit::component::text::{MarkdownExtensions, MarkdownNode, TextView, TextViewState};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, ClickEvent, Entity, Image, ImageCacheError, ImageFormat, ImageSource,
    ObjectFit, SharedString, SharedUri, Window, div, img, px,
};
use markdown::mdast::Node;
use markdown::{ParseOptions, to_mdast};
use mermaid_rs_renderer::{RenderOptions, Theme as MermaidTheme};

use crate::image_view::{MAX_IMAGE_BYTES, image_format};
use crate::theme::{color, is_dark, mono_font, ui_font};

/// Images a Markdown file shows at most, read with it.
const MAX_IMAGES: usize = 100;

/// Name of the custom node a Mermaid block becomes.
const MERMAID: &str = "mermaid";

/// Whether `path` is a Markdown file, after its extension (any case).
pub(crate) fn is_markdown(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.').is_some_and(|(_, ext)| {
        matches!(
            ext.to_ascii_lowercase().as_str(),
            "md" | "markdown" | "mdown" | "mkd" | "mkdn"
        )
    })
}

/// Where a link of a Markdown file leads.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Link {
    /// A heading of the same file, by its anchor.
    Anchor(String),
    /// A file of the repo (path from its root), at a heading or not.
    File {
        path: String,
        anchor: Option<String>,
    },
    /// A web page or address, opened in the browser.
    External(String),
}

/// Where `href`, a link of the file at `doc` (path from the repo root),
/// leads; `None` for a link going nowhere Archipel can follow (outside
/// the repo, or an unsupported scheme).
pub(crate) fn link_target(doc: &str, href: &str) -> Option<Link> {
    let href = href.trim();
    if let Some(anchor) = href.strip_prefix('#') {
        return Some(Link::Anchor(percent_decode(anchor)));
    }
    if let Some(scheme) = scheme(href) {
        let web = matches!(scheme.as_str(), "http" | "https" | "mailto");
        return web.then(|| Link::External(href.to_owned()));
    }
    let (target, anchor) = match href.split_once('#') {
        Some((target, anchor)) => (target, Some(percent_decode(anchor))),
        None => (href, None),
    };
    let anchor = anchor.filter(|a| !a.is_empty());
    if target.is_empty() {
        return anchor.map(Link::Anchor);
    }
    let path = repo_path(doc, target)?;
    if path == doc
        && let Some(anchor) = anchor
    {
        return Some(Link::Anchor(anchor));
    }
    Some(Link::File { path, anchor })
}

/// The path from the repo root of `target`, a relative reference of the
/// file at `doc` (`/`-led: from the repo root, as forges read it); `None`
/// for a URL, an anchor, or a path leaving the repo.
pub(crate) fn repo_path(doc: &str, target: &str) -> Option<String> {
    let target = target.trim();
    if target.is_empty() || target.starts_with('#') || target.starts_with("//") {
        return None;
    }
    if scheme(target).is_some() {
        return None;
    }
    let target = target.split(['?', '#']).next().unwrap_or_default();
    let target = percent_decode(target);
    let mut parts: Vec<&str> = Vec::new();
    if !target.starts_with('/') {
        parts.extend(doc.rsplit_once('/').map_or("", |(dir, _)| dir).split('/'));
    }
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.retain(|p| !p.is_empty());
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    parts.retain(|p| !p.is_empty());
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// The scheme of `href` (lowercase) when it is a URL: letters, digits,
/// `+-.` before a `:`, at least two of them (so `C:\x` is no URL).
fn scheme(href: &str) -> Option<String> {
    let (scheme, _) = href.split_once(':')?;
    let ok = scheme.len() > 1
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    ok.then(|| scheme.to_ascii_lowercase())
}

/// `text` with its `%XX` escapes decoded (left as they are when they do
/// not decode to UTF-8).
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| char::from(b).to_digit(16);
        if bytes[i] == b'%'
            && let (Some(&hi), Some(&lo)) = (bytes.get(i + 1), bytes.get(i + 2))
            && let (Some(hi), Some(lo)) = (hex(hi), hex(lo))
        {
            out.push(u8::try_from(hi * 16 + lo).unwrap_or(b'?'));
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_owned())
}

/// The Markdown syntax tree of `text`, as GitHub reads it.
fn syntax_tree(text: &str) -> Option<Node> {
    to_mdast(text, &ParseOptions::gfm()).ok()
}

/// Calls `f` on `node` and every node under it, in document order.
fn walk(node: &Node, f: &mut impl FnMut(&Node)) {
    f(node);
    for child in node.children().into_iter().flatten() {
        walk(child, f);
    }
}

/// The image sources of `text` as written: `![](…)` images, image
/// references' definitions and `<img src>` in raw HTML, first seen first.
pub(crate) fn image_refs(text: &str) -> Vec<String> {
    let Some(tree) = syntax_tree(text) else {
        return Vec::new();
    };
    let mut urls = Vec::new();
    let mut referenced = HashSet::new();
    let mut definitions = HashMap::new();
    walk(&tree, &mut |node| match node {
        Node::Image(image) => urls.push(image.url.clone()),
        Node::ImageReference(r) => {
            referenced.insert(r.identifier.clone());
        }
        Node::Definition(d) => {
            definitions.insert(d.identifier.clone(), d.url.clone());
        }
        Node::Html(html) => urls.extend(html_image_sources(&html.value)),
        _ => {}
    });
    urls.extend(
        definitions
            .into_iter()
            .filter(|(id, _)| referenced.contains(id))
            .map(|(_, url)| url),
    );
    let mut seen = HashSet::new();
    urls.retain(|u| seen.insert(u.clone()));
    urls
}

/// The `src` of each `<img>` tag of `html`.
fn html_image_sources(html: &str) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let mut sources = Vec::new();
    let mut from = 0;
    while let Some(at) = lower[from..].find("<img") {
        let start = from + at;
        let end = lower[start..].find('>').map_or(lower.len(), |e| start + e);
        if let Some(src) = attribute(&html[start..end], &lower[start..end], "src") {
            sources.push(src);
        }
        from = end;
    }
    sources
}

/// The value of attribute `name` in `tag` (`lower` is `tag` lowercased).
fn attribute(tag: &str, lower: &str, name: &str) -> Option<String> {
    let mut from = 0;
    while let Some(at) = lower[from..].find(name) {
        let start = from + at;
        from = start + name.len();
        let before = lower[..start].chars().next_back();
        if !before.is_some_and(char::is_whitespace) {
            continue;
        }
        let rest = tag[from..].trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim_start();
        let value = match rest.chars().next() {
            Some(q @ ('"' | '\'')) => rest[1..].split(q).next().unwrap_or_default(),
            _ => rest
                .split(|c: char| c.is_whitespace() || c == '/')
                .next()
                .unwrap_or_default(),
        };
        return Some(value.to_owned());
    }
    None
}

/// The images of the Markdown file at `doc` read with `read` (from the
/// same commit, or the working copy), by their source as written. Only
/// repo files are read: a web image is never fetched.
pub(crate) fn load_images(
    doc: &str,
    text: &str,
    read: impl Fn(&str) -> Option<Vec<u8>>,
) -> HashMap<String, Arc<Image>> {
    image_refs(text)
        .into_iter()
        .take(MAX_IMAGES)
        .filter_map(|url| {
            let path = repo_path(doc, &url)?;
            let format = image_format(&path)?;
            let bytes = read(&path).filter(|b| b.len() <= MAX_IMAGE_BYTES)?;
            Some((url, Arc::new(Image::from_bytes(format, bytes))))
        })
        .collect()
}

/// The anchor GitHub gives a heading: lowercase, spaces to `-`, only
/// letters, digits, `-` and `_` kept.
pub(crate) fn slug(heading: &str) -> String {
    heading
        .trim()
        .to_lowercase()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            c if c.is_alphanumeric() || c == '-' || c == '_' => Some(c),
            _ => None,
        })
        .collect()
}

/// The byte range in `text` of the heading `anchor` points to: the first
/// heading with that slug, the n-th repeat for `slug-n`, as GitHub does.
pub(crate) fn heading_range(text: &str, anchor: &str) -> Option<Range<usize>> {
    let anchor = anchor.to_lowercase();
    let tree = syntax_tree(text)?;
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut found = None;
    walk(&tree, &mut |node| {
        if found.is_some() {
            return;
        }
        let Node::Heading(_) = node else {
            return;
        };
        let base = slug(&node.to_string());
        let n = counts.entry(base.clone()).or_default();
        let slug = if *n == 0 { base } else { format!("{base}-{n}") };
        *n += 1;
        if slug == anchor {
            found = node.position().map(|p| p.start.offset..p.end.offset);
        }
    });
    found
}

/// A Mermaid block, drawn.
struct Diagram {
    /// Its source, shown when it does not draw.
    source: String,
    /// The SVG, or why it does not draw.
    svg: Result<Arc<Image>, String>,
}

/// The colors and font a diagram is drawn with, after the app theme.
#[derive(Clone, PartialEq, Eq, Hash)]
struct DiagramStyle {
    dark: bool,
    font: String,
}

impl DiagramStyle {
    fn of(cx: &App) -> Self {
        Self {
            dark: is_dark(cx),
            font: ui_font(cx),
        }
    }

    /// The Mermaid theme: Mermaid's dark or light one, on no background
    /// (the viewer's shows through), in the UI font.
    fn theme(&self) -> MermaidTheme {
        let mut theme = if self.dark {
            MermaidTheme::dark()
        } else {
            MermaidTheme::modern()
        };
        "transparent".clone_into(&mut theme.background);
        theme.font_family = format!("{}, sans-serif", self.font);
        theme
    }

    /// Tells parses of one style from another's.
    fn revision(&self) -> u64 {
        use std::hash::{Hash as _, Hasher as _};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.hash(&mut hasher);
        hasher.finish()
    }
}

/// `source`, a Mermaid diagram, drawn as SVG; the parse error when it does
/// not draw.
pub(crate) fn mermaid_svg(source: &str, dark: bool, font: &str) -> Result<String, String> {
    let style = DiagramStyle {
        dark,
        font: font.to_owned(),
    };
    let options = RenderOptions {
        theme: style.theme(),
        ..RenderOptions::default()
    };
    // A renderer bug must not take the app down with it.
    catch_unwind(AssertUnwindSafe(|| {
        mermaid_rs_renderer::render_strict(source, options)
    }))
    .map_err(|_| "The diagram could not be drawn".to_owned())?
    .map_err(|e| e.to_string())
}

/// The text view's extensions: Mermaid blocks drawn in `style`.
fn extensions(style: &DiagramStyle) -> MarkdownExtensions {
    let parse_style = style.clone();
    MarkdownExtensions::default()
        .block_parser(move |node, cx| {
            let Node::Code(code) = node else {
                return None;
            };
            if !code
                .lang
                .as_deref()
                .is_some_and(|l| l.eq_ignore_ascii_case(MERMAID))
            {
                return None;
            }
            let svg = mermaid_svg(&code.value, parse_style.dark, &parse_style.font)
                .map(|svg| Arc::new(Image::from_bytes(ImageFormat::Svg, svg.into_bytes())));
            let diagram = Diagram {
                source: code.value.clone(),
                svg,
            };
            Some(
                MarkdownNode::new(MERMAID, diagram)
                    .text(code.value.clone())
                    .markdown(cx.node_source(node).unwrap_or_default()),
            )
        })
        .block_renderer(MERMAID, |node, _window, cx| render_diagram(node, cx))
        .parser_revision(style.revision())
}

/// A Mermaid block: its diagram, or its source and why it does not draw.
fn render_diagram(node: &MarkdownNode, cx: &App) -> AnyElement {
    let Some(diagram) = node.data::<Diagram>() else {
        return div().into_any_element();
    };
    let frame = div()
        .w_full()
        .my_2()
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(color(cx, T::BorderSubtle));
    match &diagram.svg {
        Ok(svg) => frame
            .flex()
            .justify_center()
            .child(
                img(ImageSource::Image(svg.clone()))
                    .max_w_full()
                    .object_fit(ObjectFit::ScaleDown),
            )
            .into_any_element(),
        Err(error) => frame
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .text_color(color(cx, T::StatusError))
                    .child(format!("Mermaid diagram not drawn: {error}")),
            )
            .child(
                div()
                    .font_family(mono_font(cx))
                    .text_sm()
                    .whitespace_normal()
                    .children(diagram.source.lines().map(|l| {
                        div()
                            .min_h(px(16.0))
                            .child(SharedString::from(l.to_owned()))
                    })),
            )
            .into_any_element(),
    }
}

/// The source of an image that is not shown: a web image, or a file the
/// commit does not have.
fn missing_image() -> ImageSource {
    ImageSource::Custom(Arc::new(|_, _| {
        Some(Err(ImageCacheError::Io(Arc::new(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "image not read: not in the repo",
        )))))
    }))
}

/// A Markdown file, rendered.
pub(crate) struct MarkdownDoc {
    state: Entity<TextViewState>,
    /// Its source, for the headings its links point to.
    text: Arc<str>,
    images: Arc<HashMap<String, Arc<Image>>>,
    /// The extensions for the current theme; rebuilt when it changes.
    extensions: Option<(DiagramStyle, MarkdownExtensions)>,
    /// A heading to scroll to once the text is laid out.
    pending_anchor: Option<String>,
}

impl MarkdownDoc {
    pub(crate) fn new(text: String, images: HashMap<String, Arc<Image>>, cx: &mut App) -> Self {
        let state = cx.new(|cx| TextViewState::markdown(&text, cx).scrollable(true));
        Self {
            state,
            text: text.into(),
            images: Arc::new(images),
            extensions: None,
            pending_anchor: None,
        }
    }

    /// Scrolls to the heading `anchor` points to, once it is laid out.
    pub(crate) fn scroll_to(&mut self, anchor: String, cx: &mut App) {
        self.pending_anchor = Some(anchor);
        self.reveal_pending(cx);
    }

    /// Before each render: follows the theme, and scrolls to the heading
    /// asked for if the text is laid out now.
    pub(crate) fn prepare(&mut self, cx: &mut App) {
        let style = DiagramStyle::of(cx);
        if self.extensions.as_ref().is_none_or(|(s, _)| *s != style) {
            let extensions = extensions(&style);
            self.extensions = Some((style, extensions));
        }
        self.reveal_pending(cx);
    }

    fn reveal_pending(&mut self, cx: &mut App) {
        let Some(anchor) = &self.pending_anchor else {
            return;
        };
        let Some(source) = heading_range(&self.text, anchor) else {
            self.pending_anchor = None; // no such heading
            return;
        };
        let done = self.state.update(cx, |state, cx| {
            let text = state.rendered_text();
            match text.range_for_source(source) {
                Some(range) => {
                    let _ = state.reveal_range(range.start..range.start, cx);
                    true
                }
                // Not parsed yet, unless something is shown already.
                None => !text.is_empty(),
            }
        });
        if done {
            self.pending_anchor = None;
        }
    }

    /// The rendered file; `on_link` follows a clicked link (its target as
    /// written).
    pub(crate) fn render(
        &self,
        on_link: impl Fn(&SharedString, &mut Window, &mut App) + Send + Sync + 'static,
    ) -> AnyElement {
        let images = self.images.clone();
        let mut view = TextView::new(&self.state)
            .scrollable(true)
            .image_source(move |uri: &SharedUri| {
                images
                    .get(uri.as_ref())
                    .map_or_else(missing_image, |i| ImageSource::Image(i.clone()))
            })
            .on_link_click(move |href, _: &ClickEvent, window, cx| on_link(href, window, cx));
        if let Some((_, extensions)) = &self.extensions {
            view = view.markdown_extensions(extensions.clone());
        }
        div()
            .id("markdown-view")
            .flex_1()
            .min_h_0()
            .size_full()
            .px_4()
            .child(view)
            .into_any_element()
    }
}

// Needs a headless renderer, which gpui only has on Linux and macOS.
#[cfg(test)]
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod render_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_files() {
        assert!(is_markdown("README.md"));
        assert!(is_markdown("docs/Guide.MARKDOWN"));
        assert!(!is_markdown("md"));
        assert!(!is_markdown("src/md.rs"));
    }

    #[test]
    fn links_resolve_from_the_file_folder() {
        let file = |path: &str, anchor: Option<&str>| {
            Some(Link::File {
                path: path.to_owned(),
                anchor: anchor.map(str::to_owned),
            })
        };
        assert_eq!(
            link_target("docs/UI.md", "THEMES.md"),
            file("docs/THEMES.md", None)
        );
        assert_eq!(
            link_target("docs/UI.md", "./adr/0001.md#how"),
            file("docs/adr/0001.md", Some("how"))
        );
        assert_eq!(
            link_target("docs/UI.md", "../README.md"),
            file("README.md", None)
        );
        assert_eq!(
            link_target("docs/UI.md", "/src/lib.rs?x=1"),
            file("src/lib.rs", None)
        );
        assert_eq!(
            link_target("README.md", "docs/My%20File.md"),
            file("docs/My File.md", None)
        );
        assert_eq!(link_target("README.md", "../outside.md"), None);
    }

    #[test]
    fn anchors_and_web_links() {
        assert_eq!(
            link_target("docs/UI.md", "#a-quick-look"),
            Some(Link::Anchor("a-quick-look".into()))
        );
        assert_eq!(
            link_target("docs/UI.md", "UI.md#caf%C3%A9"),
            Some(Link::Anchor("café".into()))
        );
        assert_eq!(
            link_target("README.md", "https://example.com/a"),
            Some(Link::External("https://example.com/a".into()))
        );
        assert_eq!(
            link_target("README.md", "mailto:a@b.c"),
            Some(Link::External("mailto:a@b.c".into()))
        );
        assert_eq!(link_target("README.md", "file:///etc/passwd"), None);
        assert_eq!(link_target("README.md", "javascript:alert(1)"), None);
    }

    #[test]
    fn image_sources_of_every_kind() {
        let text = "![logo](assets/logo.png)\n\n\
                    ![ref][shot]\n\n\
                    <p align=\"center\"><img alt=x src=\"assets/a.svg\" width=\"64\"></p>\n\n\
                    ![logo again](assets/logo.png)\n\n\
                    [shot]: docs/shot.png\n\
                    [link]: docs/not-an-image.md\n";
        assert_eq!(
            image_refs(text),
            ["assets/logo.png", "assets/a.svg", "docs/shot.png"]
        );
    }

    #[test]
    fn images_are_read_from_the_repo_only() {
        let text = "![a](img/a.png) ![b](https://example.com/b.png) ![c](../c.png) ![d](d.txt)";
        let read = |path: &str| (path == "docs/img/a.png").then(|| vec![1, 2, 3]);
        let images = load_images("docs/guide.md", text, read);
        assert_eq!(images.keys().collect::<Vec<_>>(), ["img/a.png"]);
        assert_eq!(images["img/a.png"].bytes(), [1, 2, 3]);
    }

    #[test]
    fn heading_anchors_as_github_makes_them() {
        assert_eq!(slug("A quick look"), "a-quick-look");
        assert_eq!(slug("feat: `Bisect` (v2)!"), "feat-bisect-v2");
        let text = "# Title\n\nIntro\n\n## Notes\n\nx\n\n## Notes\n\ny\n";
        let at = |anchor| heading_range(text, anchor).map(|r| &text[r]);
        assert_eq!(at("title"), Some("# Title"));
        assert_eq!(at("notes"), Some("## Notes"));
        assert_eq!(
            heading_range(text, "notes-1").map(|r| r.start),
            text.rfind("## Notes")
        );
        assert_eq!(at("missing"), None);
    }

    #[test]
    fn mermaid_draws_or_says_why_not() {
        let svg = mermaid_svg(
            "flowchart LR\n  A[Start] --> B{Ok?}\n  B -->|yes| C",
            true,
            "Inter",
        )
        .expect("a flowchart draws");
        assert!(svg.contains("<svg"));
        assert!(svg.contains("Start"));
        for diagram in [
            "sequenceDiagram\n  Alice->>Bob: Hi",
            "classDiagram\n  Animal <|-- Duck",
            "stateDiagram-v2\n  [*] --> Still",
            "erDiagram\n  CUSTOMER ||--o{ ORDER : places",
            "gantt\n  title Plan\n  section A\n  Task :a1, 2026-01-01, 3d",
            "pie\n  \"A\" : 1\n  \"B\" : 2",
        ] {
            assert!(mermaid_svg(diagram, false, "Inter").is_ok(), "{diagram}");
        }
        assert!(mermaid_svg("flowchart LR\n  A -->", false, "Inter").is_err());
    }
}
