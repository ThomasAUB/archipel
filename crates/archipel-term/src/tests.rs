//! Real terminals running `sh` (Unix only: Windows CI has no `sh` on a
//! `ConPTY` by default).
#![cfg(unix)]

use std::sync::mpsc;
use std::time::{Duration, Instant};

use archipel_model::terminal::{SelectionKind, ShellCommand};

use super::*;

/// Launches `script` under `sh -c`, its events on the returned channel.
fn run(script: &str) -> (Arc<dyn TerminalSession>, mpsc::Receiver<TerminalEvent>) {
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let sink = move |e: TerminalEvent| {
        let _ = tx.lock().unwrap().send(e);
    };
    let spec = TerminalSpec {
        cwd: std::env::temp_dir(),
        command: Some(ShellCommand {
            program: "sh".into(),
            args: vec!["-c".into(), script.into()],
        }),
        size: TerminalSize {
            cols: 40,
            rows: 5,
            ..TerminalSize::default()
        },
        scrollback: 100,
        env: vec![("ARCHIPEL_TEST".into(), "on".into())],
    };
    let session = PtyTerminals::new().launch(spec, Arc::new(sink)).unwrap();
    (session, rx)
}

/// Waits for the exit event and returns its code.
fn exit_code(rx: &mpsc::Receiver<TerminalEvent>) -> Option<i32> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if let TerminalEvent::Exited(code) =
            rx.recv_timeout(left).expect("the program did not exit")
        {
            return code;
        }
    }
}

fn lines(screen: &TerminalScreen) -> Vec<String> {
    (0..screen.rows)
        .map(|r| screen.row_text(r).trim_end().to_owned())
        .collect()
}

#[test]
fn output_colors_and_exit_code_reach_the_frontend() {
    let (session, rx) =
        run("printf 'hello %s\\n' \"$ARCHIPEL_TEST\"; printf '\\033[1;31mred\\033[0m'; exit 3");
    assert_eq!(exit_code(&rx), Some(3));
    let screen = session.screen();
    assert_eq!(screen.cols, 40);
    let text = lines(&screen);
    assert_eq!(text[0], "hello on");
    assert_eq!(text[1], "red");
    let r = screen.row(1)[0];
    assert_eq!(r.fg, TermColor::Indexed(1));
    assert!(r.flags.contains(archipel_model::terminal::CellFlags::BOLD));
}

#[test]
fn input_reaches_the_program() {
    let (session, rx) = run("read line; printf 'got:%s' \"$line\"");
    session.write(b"abc\r");
    assert_eq!(exit_code(&rx), Some(0));
    assert!(
        lines(&session.screen())
            .iter()
            .any(|l| l.contains("got:abc"))
    );
}

#[test]
fn scrollback_selection_and_resize() {
    let (session, rx) = run("for i in 1 2 3 4 5 6 7 8; do echo line$i; done");
    assert_eq!(exit_code(&rx), Some(0));
    let screen = session.screen();
    assert!(screen.history > 0);
    assert_eq!(screen.scrolled_back, 0);

    session.scroll(TerminalScroll::Top);
    let top = session.screen();
    assert_eq!(top.scrolled_back, top.history);
    assert_eq!(top.row_text(0).trim_end(), "line1");
    assert!(top.cursor.is_none() || top.history < usize::from(top.rows));

    session.select(SelectionUpdate::Start {
        at: ScreenPoint { row: 0, col: 1 },
        kind: SelectionKind::Word,
        right_half: false,
    });
    assert_eq!(session.selected_text().as_deref(), Some("line1"));
    assert!(
        session.screen().row(0)[0]
            .flags
            .contains(archipel_model::terminal::CellFlags::SELECTED)
    );
    session.select(SelectionUpdate::Clear);
    assert_eq!(session.selected_text(), None);

    session.scroll(TerminalScroll::Bottom);
    session.resize(TerminalSize {
        cols: 20,
        rows: 3,
        ..TerminalSize::default()
    });
    let small = session.screen();
    assert_eq!((small.cols, small.rows), (20, 3));
    assert_eq!(small.cells.len(), 60);
}

use archipel_model::terminal::TermColor;
