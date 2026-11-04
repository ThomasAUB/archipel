//! The semantic color token set: typed identifiers with stable string ids,
//! grouping metadata for the advanced editor, and [`ColorSet`], a complete
//! token → color table.

use std::fmt;
use std::ops::Index;

use crate::color::Color;

/// Number of graph lane colors in the palette.
pub const LANE_COUNT: usize = 10;

/// Group a token belongs to; the advanced editor lists tokens by group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TokenGroup {
    /// Backgrounds and surfaces.
    Surface,
    /// Text colors.
    Text,
    /// Accent color and its companions.
    Accent,
    /// Borders and focus rings.
    Border,
    /// Repository / operation status colors.
    Status,
    /// Diff viewer colors.
    Diff,
    /// Commit graph lane palette.
    Graph,
    /// Syntax highlighting.
    Syntax,
}

impl TokenGroup {
    /// All groups in display order.
    pub const ALL: [Self; 8] = [
        Self::Surface,
        Self::Text,
        Self::Accent,
        Self::Border,
        Self::Status,
        Self::Diff,
        Self::Graph,
        Self::Syntax,
    ];

    /// Human-readable group label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Surface => "Surfaces",
            Self::Text => "Text",
            Self::Accent => "Accent",
            Self::Border => "Borders",
            Self::Status => "Status",
            Self::Diff => "Diff",
            Self::Graph => "Graph lanes",
            Self::Syntax => "Syntax highlighting",
        }
    }

    /// Tokens of this group, in display order.
    pub fn tokens(self) -> impl Iterator<Item = ColorToken> {
        ColorToken::ALL
            .iter()
            .copied()
            .filter(move |t| t.group() == self)
    }
}

/// Metadata describing one token, for listing in the editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenInfo {
    /// The token.
    pub token: ColorToken,
    /// Stable string id used in theme files (e.g. `"bg.app"`).
    pub id: &'static str,
    /// Group the token belongs to.
    pub group: TokenGroup,
    /// Human-readable label.
    pub label: &'static str,
}

macro_rules! color_tokens {
    ($( $variant:ident => $id:literal, $group:ident, $label:literal; )*) => {
        /// A semantic color token. UI code reads colors only through these.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum ColorToken {
            $( #[doc = $label] $variant, )*
        }

        impl ColorToken {
            /// Every token, in display order.
            pub const ALL: &'static [Self] = &[ $( Self::$variant, )* ];
            /// Number of tokens.
            pub const COUNT: usize = Self::ALL.len();

            /// Stable string id used in theme files and the advanced editor.
            #[must_use]
            pub const fn id(self) -> &'static str {
                match self { $( Self::$variant => $id, )* }
            }

            /// Group the token belongs to.
            #[must_use]
            pub const fn group(self) -> TokenGroup {
                match self { $( Self::$variant => TokenGroup::$group, )* }
            }

            /// Human-readable label.
            #[must_use]
            pub const fn label(self) -> &'static str {
                match self { $( Self::$variant => $label, )* }
            }
        }
    };
}

color_tokens! {
    BgApp => "bg.app", Surface, "App background";
    BgPanel => "bg.panel", Surface, "Panel background";
    BgElevated => "bg.elevated", Surface, "Elevated surface (menus, popovers)";
    BgInput => "bg.input", Surface, "Input field background";
    BgHover => "bg.hover", Surface, "Hovered item background";
    BgSelected => "bg.selected", Surface, "Selected item background";
    FgDefault => "fg.default", Text, "Default text";
    FgMuted => "fg.muted", Text, "Muted text";
    FgSubtle => "fg.subtle", Text, "Subtle text (hints, placeholders)";
    FgDisabled => "fg.disabled", Text, "Disabled text";
    Accent => "accent", Accent, "Accent";
    AccentHover => "accent.hover", Accent, "Accent (hovered)";
    AccentFg => "accent.fg", Accent, "Text on accent";
    AccentSubtle => "accent.subtle", Accent, "Subtle accent background";
    Border => "border.default", Border, "Border";
    BorderSubtle => "border.subtle", Border, "Subtle border (separators)";
    BorderFocus => "border.focus", Border, "Focus ring";
    StatusDirty => "status.dirty", Status, "Dirty (uncommitted changes)";
    StatusAhead => "status.ahead", Status, "Ahead of upstream";
    StatusBehind => "status.behind", Status, "Behind upstream";
    StatusConflict => "status.conflict", Status, "Conflict";
    StatusSuccess => "status.success", Status, "Success";
    StatusError => "status.error", Status, "Error";
    StatusWarning => "status.warning", Status, "Warning";
    DiffAddBg => "diff.add.bg", Diff, "Added line background";
    DiffDelBg => "diff.del.bg", Diff, "Deleted line background";
    DiffAddWordBg => "diff.add.word.bg", Diff, "Added word highlight";
    DiffDelWordBg => "diff.del.word.bg", Diff, "Deleted word highlight";
    DiffAddFg => "diff.add.fg", Diff, "Added text / gutter marker";
    DiffDelFg => "diff.del.fg", Diff, "Deleted text / gutter marker";
    GraphLane0 => "graph.lane.0", Graph, "Lane 1";
    GraphLane1 => "graph.lane.1", Graph, "Lane 2";
    GraphLane2 => "graph.lane.2", Graph, "Lane 3";
    GraphLane3 => "graph.lane.3", Graph, "Lane 4";
    GraphLane4 => "graph.lane.4", Graph, "Lane 5";
    GraphLane5 => "graph.lane.5", Graph, "Lane 6";
    GraphLane6 => "graph.lane.6", Graph, "Lane 7";
    GraphLane7 => "graph.lane.7", Graph, "Lane 8";
    GraphLane8 => "graph.lane.8", Graph, "Lane 9";
    GraphLane9 => "graph.lane.9", Graph, "Lane 10";
    SyntaxKeyword => "syntax.keyword", Syntax, "Keyword";
    SyntaxString => "syntax.string", Syntax, "String";
    SyntaxComment => "syntax.comment", Syntax, "Comment";
    SyntaxFunction => "syntax.function", Syntax, "Function";
    SyntaxType => "syntax.type", Syntax, "Type";
    SyntaxNumber => "syntax.number", Syntax, "Number";
    SyntaxConstant => "syntax.constant", Syntax, "Constant";
    SyntaxOperator => "syntax.operator", Syntax, "Operator / punctuation";
}

impl ColorToken {
    /// The graph lane tokens, in palette order.
    pub const LANES: [Self; LANE_COUNT] = [
        Self::GraphLane0,
        Self::GraphLane1,
        Self::GraphLane2,
        Self::GraphLane3,
        Self::GraphLane4,
        Self::GraphLane5,
        Self::GraphLane6,
        Self::GraphLane7,
        Self::GraphLane8,
        Self::GraphLane9,
    ];

    /// Looks a token up by its string id.
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|t| t.id() == id)
    }

    /// Lane token for lane `index` (wraps around the palette).
    #[must_use]
    pub const fn lane(index: usize) -> Self {
        Self::LANES[index % LANE_COUNT]
    }

    /// Full metadata.
    #[must_use]
    pub const fn info(self) -> TokenInfo {
        TokenInfo {
            token: self,
            id: self.id(),
            group: self.group(),
            label: self.label(),
        }
    }

    const fn index(self) -> usize {
        self as usize
    }
}

impl fmt::Display for ColorToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

/// Tokens grouped for the advanced editor, in display order.
#[must_use]
pub fn grouped_tokens() -> Vec<(TokenGroup, Vec<TokenInfo>)> {
    TokenGroup::ALL
        .iter()
        .map(|&g| (g, g.tokens().map(ColorToken::info).collect()))
        .collect()
}

/// A complete table assigning a color to every [`ColorToken`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ColorSet {
    colors: [Color; ColorToken::COUNT],
}

impl ColorSet {
    /// A set with every token set to `color`.
    #[must_use]
    pub const fn filled(color: Color) -> Self {
        Self {
            colors: [color; ColorToken::COUNT],
        }
    }

    /// Color of `token`.
    #[must_use]
    pub const fn get(&self, token: ColorToken) -> Color {
        self.colors[token.index()]
    }

    /// Sets the color of `token`.
    pub const fn set(&mut self, token: ColorToken, color: Color) {
        self.colors[token.index()] = color;
    }

    /// Color of graph lane `index` (wraps around the palette).
    #[must_use]
    pub const fn lane(&self, index: usize) -> Color {
        self.get(ColorToken::lane(index))
    }

    /// All `(token, color)` pairs in display order.
    pub fn iter(&self) -> impl Iterator<Item = (ColorToken, Color)> + '_ {
        ColorToken::ALL.iter().map(|&t| (t, self.get(t)))
    }
}

impl Index<ColorToken> for ColorSet {
    type Output = Color;
    fn index(&self, token: ColorToken) -> &Color {
        &self.colors[token.index()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_round_trip() {
        for (i, t) in ColorToken::ALL.iter().enumerate() {
            assert_eq!(t.index(), i);
            assert_eq!(ColorToken::from_id(t.id()), Some(*t));
        }
        let mut ids: Vec<_> = ColorToken::ALL.iter().map(|t| t.id()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), ColorToken::COUNT);
    }

    #[test]
    fn every_token_is_in_exactly_one_group() {
        let total: usize = grouped_tokens().iter().map(|(_, v)| v.len()).sum();
        assert_eq!(total, ColorToken::COUNT);
        assert_eq!(TokenGroup::Graph.tokens().count(), LANE_COUNT);
    }
}
