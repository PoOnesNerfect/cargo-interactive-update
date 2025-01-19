use std::{io::stdout, time::Duration};

use clap::Parser;
use crossterm::{
    cursor::{MoveToNextLine, Show},
    execute,
    style::{Print, ResetColor},
    terminal::disable_raw_mode,
};

mod api;
mod args;
mod cargo;
mod cli;
mod dependency;
mod info;
mod loading;
mod search;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args::CargoCli::InteractiveUpdate(args) = args::CargoCli::parse();

    let theme = std::thread::spawn(|| {
        termbg::theme(Duration::from_millis(500)).unwrap_or(termbg::Theme::Light)
    });

    let dependencies = cargo::CargoDependencies::gather_dependencies();
    let total_deps = dependencies.len();

    let loader = loading::init_loader(total_deps).unwrap();

    let outdated_deps = dependencies.retrieve_outdated_dependencies(None, loader);
    let total_outdated_deps = outdated_deps.len();

    if total_outdated_deps == 0 {
        execute!(
            stdout(),
            MoveToNextLine(1),
            Print(format!(
                "All {total_deps} direct dependencies are up to date!"
            )),
            Show,
            ResetColor
        )?;
        disable_raw_mode()?;
        return Ok(());
    }

    println!("{total_outdated_deps} out of the {total_deps} direct dependencies are outdated.");

    let mut state = cli::State::new(
        outdated_deps,
        total_deps,
        args.all,
        theme.join().expect("operation in thread failed"),
    );

    if args.yes {
        state.selected_dependencies().apply_versions(args)?;
        return Ok(());
    }

    state.start()?;

    loop {
        match state.handle_keyboard_event()? {
            cli::Event::HandleKeyboard => {}
            cli::Event::UpdateDependencies => {
                state.selected_dependencies().apply_versions(args)?;
                break;
            }
            cli::Event::Exit => {
                break;
            }
        }
    }

    Ok(())
}
