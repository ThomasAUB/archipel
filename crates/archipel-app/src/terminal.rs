//! The integrated terminal (issue #98): which shell runs, how a one-off
//! command is passed to it, and what a terminal tab is called. Starting
//! the terminal itself goes through the [`TerminalLauncher`] port of
//! [`Services::terminals`](crate::Services::terminals), with
//! `App::terminal_spec` and `App::launch_terminal`.
//!
//! [`TerminalLauncher`]: archipel_model::ports::TerminalLauncher

use archipel_model::terminal::ShellCommand;

/// Splits a `terminal_shell` setting into a program and its arguments:
/// words split on blanks, `"…"` or `'…'` keep blanks. `None` when blank.
#[must_use]
pub fn parse_shell(text: &str) -> Option<ShellCommand> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quote = None;
    for c in text.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => word.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                in_word = true;
            }
            (None, c) if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            (None, c) => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(word);
    }
    let mut words = words.into_iter();
    let program = words.next()?;
    Some(ShellCommand {
        program,
        args: words.collect(),
    })
}

/// The user's shell when none is set: `$SHELL` (else `/bin/sh`), or
/// PowerShell on Windows.
#[must_use]
pub fn default_shell() -> ShellCommand {
    let program = if cfg!(windows) {
        "powershell.exe".to_owned()
    } else {
        std::env::var("SHELL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "/bin/sh".to_owned())
    };
    ShellCommand {
        program,
        args: Vec::new(),
    }
}

/// `shell` running `command` and exiting with its status: `/C` for
/// `cmd`, `-Command` for PowerShell, `-c` for the others (sh, bash, zsh,
/// fish, nu…).
#[must_use]
pub fn run_command(shell: &ShellCommand, command: &str) -> ShellCommand {
    // Either separator: a Windows path is written with `\\`.
    let file = shell.program.rsplit(['/', '\\']).next().unwrap_or_default();
    let name = file.to_ascii_lowercase();
    let name = name.strip_suffix(".exe").unwrap_or(&name);
    let flag = match name {
        "cmd" => "/C",
        "powershell" | "pwsh" => "-Command",
        _ => "-c",
    };
    let mut args = shell.args.clone();
    args.push(flag.to_owned());
    args.push(command.to_owned());
    ShellCommand {
        program: shell.program.clone(),
        args,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(program: &str, args: &[&str]) -> ShellCommand {
        ShellCommand {
            program: program.into(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
        }
    }

    #[test]
    fn shell_settings_split_into_words() {
        assert_eq!(parse_shell("  "), None);
        assert_eq!(parse_shell("bash"), Some(cmd("bash", &[])));
        assert_eq!(
            parse_shell("pwsh -NoLogo  -NoProfile"),
            Some(cmd("pwsh", &["-NoLogo", "-NoProfile"]))
        );
        assert_eq!(
            parse_shell(r#""C:\Program Files\Git\bin\bash.exe" --login -i"#),
            Some(cmd(
                r"C:\Program Files\Git\bin\bash.exe",
                &["--login", "-i"]
            ))
        );
        assert_eq!(
            parse_shell("sh -c 'a b' \"\""),
            Some(cmd("sh", &["-c", "a b", ""]))
        );
    }

    #[test]
    fn commands_use_each_shell_s_own_flag() {
        assert_eq!(
            run_command(&cmd("/bin/zsh", &[]), "git status"),
            cmd("/bin/zsh", &["-c", "git status"])
        );
        assert_eq!(
            run_command(&cmd("cmd.exe", &[]), "dir"),
            cmd("cmd.exe", &["/C", "dir"])
        );
        assert_eq!(
            run_command(&cmd(r"C:\x\PowerShell.EXE", &["-NoLogo"]), "ls"),
            cmd(r"C:\x\PowerShell.EXE", &["-NoLogo", "-Command", "ls"])
        );
        assert_eq!(
            run_command(&cmd("pwsh", &[]), "ls"),
            cmd("pwsh", &["-Command", "ls"])
        );
    }
}
