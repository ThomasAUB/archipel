# Extending Archipel

Archipel is built to be forked: every customizable behavior is a small trait
registered in a registry or chosen in the composition root, and the built-in
features use exactly the same extension points (ARCHITECTURE §5). You add an
implementation; you never patch the core.

| What | Trait | Where it is registered |
|---|---|---|
| Dashboard columns | `archipel_app::dashboard::DashboardColumn` | `Extensions::columns` (`ColumnRegistry`) |
| Dashboard filters | `archipel_app::dashboard::RepoFilter` | `Extensions::filters` (`FilterRegistry`) |
| Palette / menu actions | `archipel_app::Action` (or `SimpleAction`) | `Extensions::actions` (`ActionRegistry`) |
| Shortcuts | keymap file `keymap.toml` | user config dir |
| "Open in browser" links | `archipel_app::forge::ForgeProvider` | `ForgeRegistry::register` |
| Ticket prefix | `archipel_app::message::TicketPrefixExtractor` | per-project settings |
| Commit message templates | `archipel_app::message::MessageTemplate` | per-project settings |
| Statistics | `archipel_stats::StatProvider` | `StatsRegistry::register` |
| Look and feel | theme tokens and theme files | `themes/*.toml` in the user config dir |
| Git backend per operation | the reader/writer traits of `archipel_model::ports::git` | `archipel_compose::git_ports` |
| Persistence | the store traits of `archipel_model::ports::store` | `archipel_compose::compose` |
| File watching | `archipel_model::ports::ChangeSource` | `archipel_compose::compose` |

Registering an implementation with the id of a built-in replaces the
built-in in place.

## A dashboard column

```rust
use std::sync::Arc;
use archipel_app::dashboard::{Cell, DashboardColumn, DashboardRow, SortKey, Tone};
use archipel_model::Timestamp;

struct Stashes;

impl DashboardColumn for Stashes {
    fn id(&self) -> &str { "stashes" }
    fn header(&self) -> &str { "Stashes" }
    fn cell(&self, row: &DashboardRow, _now: Timestamp) -> Cell {
        let n = row.summary().map_or(0, |s| s.stash_count);
        Cell { text: n.to_string(), detail: None, tone: Tone::Muted }
    }
    fn sort_key(&self, row: &DashboardRow) -> SortKey {
        row.summary().map_or(SortKey::None, |s| SortKey::Number(s.stash_count.into()))
    }
}

// In the composition root, before `App::new`:
// let mut ext = Extensions::default();
// ext.columns.register(Arc::new(Stashes));
```

## A filter

```rust
use archipel_app::dashboard::{DashboardRow, RepoFilter};

struct OnMain;
impl RepoFilter for OnMain {
    fn id(&self) -> &str { "on-main" }
    fn label(&self) -> &str { "On main" }
    fn matches(&self, row: &DashboardRow) -> bool {
        row.summary().and_then(|s| s.head.branch()) == Some("main")
    }
}
// ext.filters.register(Arc::new(OnMain));
```

## An action and its shortcut

```rust
use archipel_app::{Command, actions::SimpleAction};

let refresh = SimpleAction::new(
    "my.refresh_twice", "Refresh everything",
    |ctx| ctx.project_open,
    |_| Some(Command::RefreshAll),
);
// ext.actions.register(Arc::new(refresh));
```

Then bind it in `keymap.toml` (user config dir):

```toml
"ctrl-shift-r" = "my.refresh_twice"
"ctrl-j" = ""          # unbind a default shortcut
```

## A self-hosted forge

```rust
use archipel_app::forge::{ForgeLinks, ForgeProvider, RemoteLocation};

struct Gitea;
impl ForgeProvider for Gitea {
    fn id(&self) -> &str { "gitea" }
    fn links(&self, remote: &RemoteLocation) -> Option<Box<dyn ForgeLinks>> {
        (remote.host == "git.example.com").then(|| todo!("build links"))
    }
}
// Arc::get_mut(&mut ext.forges).unwrap().register(Arc::new(Gitea));
```

## A statistic

Implement `archipel_stats::StatProvider` (`id`, `label`, `compute`,
`aggregate`) reading from `StatSources` (the incremental store, refs and
history), return a `StatView::Table` for a generic table view, and register it
with `StatsRegistry::register`.

## A theme

Create `themes/ocean.toml` in the user config dir (or use Settings →
Appearance, which writes the same file):

```toml
name = "Ocean"
extends = "dark"

[base]                 # optional: re-derive every color from three
background = "#0b1d2a"
text = "#d8e6f0"
accent = "#3fb6d3"

[tokens]               # optional: override individual tokens
"status.dirty" = "#f5a524"
"graph.lane.0" = "#58c4dc"

[typography]
ui_font = "Inter"
mono_font = "JetBrains Mono"
size = 14
```

Unknown or invalid entries produce warnings and fall back to the parent
theme; a broken file never prevents the app from starting.

## Swapping a backend

`archipel_compose::git_ports` decides which adapter serves each Git port.
For example, to serve history from the CLI while keeping status on `gix`:

```rust
let mut ports = git_ports(&settings, &cli);
ports.history = cli.clone();
```

Consumers only see the traits, so nothing else changes.
