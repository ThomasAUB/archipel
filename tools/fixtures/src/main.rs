//! Command-line entry point of the fixture generator.
//!
//! ```text
//! archipel-fixtures project <dir> [repos] [commits]
//! archipel-fixtures history <dir> [commits]
//! ```

use std::path::PathBuf;

use anyhow::{Result, bail};
use archipel_fixtures::history::{HistoryShape, generate_history};
use archipel_fixtures::project::generate_project;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let num = |i: usize, default: usize| -> Result<usize> {
        Ok(args
            .get(i)
            .map(|s| s.parse())
            .transpose()?
            .unwrap_or(default))
    };
    match (args.first().map(String::as_str), args.get(1)) {
        (Some("project"), Some(dir)) => {
            let repos = generate_project(&PathBuf::from(dir), num(2, 20)?, num(3, 30)?)?;
            println!("generated {} repositories in {dir}", repos.len());
        }
        (Some("history"), Some(dir)) => {
            let shape = HistoryShape {
                commits: num(2, 100_000)?,
                ..HistoryShape::default()
            };
            generate_history(&PathBuf::from(dir), shape)?;
            println!("generated a {}-commit history in {dir}", shape.commits);
        }
        _ => bail!(
            "usage: archipel-fixtures (project <dir> [repos] [commits] | history <dir> [commits])"
        ),
    }
    Ok(())
}
