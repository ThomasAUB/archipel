//! Opening a worktree elsewhere (WORKTREES.md §6.3): its folder in the
//! file manager, a terminal or an external editor, from the user's settings
//! (`terminal_command`, `editor_command`). These are the hand-off points to
//! an agent.

use std::path::Path;
use std::process::Command;

/// Splits a command line on whitespace, honoring double quotes.
fn split_command(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in line.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Runs `line` with `dir` appended, in `dir`, detached.
fn spawn_line(line: &str, dir: &Path) -> Result<(), String> {
    let parts = split_command(line);
    let (program, args) = parts
        .split_first()
        .ok_or_else(|| "the command is empty".to_owned())?;
    let mut cmd = if cfg!(windows) {
        // `cmd` finds `.cmd` / `.bat` launchers (`code`), as a shell would.
        let mut c = Command::new("cmd");
        c.arg("/C").arg(program);
        c
    } else {
        Command::new(program)
    };
    cmd.args(args)
        .arg(dir)
        .current_dir(dir)
        .spawn()
        .map(drop)
        .map_err(|e| format!("cannot run {program}: {e}"))
}

/// Opens a terminal in `dir`: `command` (the folder appended) or the
/// platform's terminal.
pub fn open_terminal(dir: &Path, command: Option<&str>) -> Result<(), String> {
    if let Some(line) = command {
        return spawn_line(line, dir);
    }
    let result = if cfg!(windows) {
        // A new console window starting in `dir`.
        Command::new("cmd")
            .args(["/C", "start", "", "cmd", "/K"])
            .current_dir(dir)
            .spawn()
    } else if cfg!(target_os = "macos") {
        Command::new("open")
            .args(["-a", "Terminal"])
            .arg(dir)
            .spawn()
    } else {
        Command::new("x-terminal-emulator").current_dir(dir).spawn()
    };
    result
        .map(drop)
        .map_err(|e| format!("cannot open a terminal: {e} (set `terminal_command`)"))
}

/// Opens `dir` in the external editor: `command` (the folder appended), or
/// `code`.
pub fn open_editor(dir: &Path, command: Option<&str>) -> Result<(), String> {
    spawn_line(command.unwrap_or("code"), dir).map_err(|e| format!("{e} (set `editor_command`)"))
}

#[cfg(test)]
mod tests {
    use super::split_command;

    #[test]
    fn splits_quoted_arguments() {
        assert_eq!(split_command("code -n"), ["code", "-n"]);
        assert_eq!(
            split_command(r#""C:\Program Files\x\x.exe" --new"#),
            [r"C:\Program Files\x\x.exe", "--new"]
        );
        assert!(split_command("  ").is_empty());
    }
}
