use std::{
    io::{stdout, Stdout, Write},
    ops::DerefMut,
    sync::{Arc, Mutex},
};

use crossterm::{
    cursor::{Hide, MoveTo, MoveToColumn, MoveToNextLine},
    queue,
    style::Print,
    terminal::{enable_raw_mode, Clear, ClearType},
};

#[derive(Debug, Clone)]
pub struct Loader(Arc<Mutex<LoaderInner>>);

#[derive(Debug)]
struct LoaderInner {
    total_deps: usize,
    loaded_deps: usize,
    stdout: Stdout,
    cols: usize,
    digits: u8,
}

pub fn init_loader(
    total_deps: usize,
) -> Result<Loader, Box<dyn std::error::Error>> {
    let digits = length(total_deps as u32, 10) as u8;
    let mut state = LoaderInner {
        total_deps,
        loaded_deps: 0,
        stdout: stdout(),
        cols: total_deps.min(
            crossterm::terminal::size().unwrap().0 as usize
                - 2
                - digits as usize * 2
                - 7,
        ),
        digits,
    };

    enable_raw_mode()?;
    queue!(state.stdout, Hide, Clear(ClearType::All))?;
    queue!(
        state.stdout,
        MoveTo(0, 0),
        Print("Scanning Dependencies"),
        MoveToNextLine(1),
        Print(format!(
            "[{}] {:>width$}/{total_deps}   0%",
            "-".repeat(state.cols),
            0,
            width = state.digits as usize
        ))
    )?;

    state.stdout.flush()?;

    Ok(Loader(Arc::new(Mutex::new(state))))
}

impl Loader {
    pub fn inc_loader(&self) {
        let mut state = self.0.lock().unwrap();
        let LoaderInner {
            total_deps,
            loaded_deps,
            stdout,
            cols,
            digits,
        } = &mut state.deref_mut();
        *loaded_deps += 1;

        let index = 10000 * *cols * (*loaded_deps - 1) / *total_deps / 10000;
        let perc = 100 * *loaded_deps / *total_deps;

        queue!(
            stdout,
            MoveTo((index + 1) as u16, 1),
            Print("="),
            MoveToColumn(*cols as u16 + 3),
            Print(format!(
                "{:>width$}/{total_deps} {perc:>3}%",
                loaded_deps,
                width = *digits as usize
            )),
        )
        .unwrap();

        stdout.flush().unwrap();
    }
}

fn length(n: u32, base: u32) -> u32 {
    let mut power = base;
    let mut count = 1;
    while n >= power {
        count += 1;
        if let Some(new_power) = power.checked_mul(base) {
            power = new_power;
        } else {
            break;
        }
    }
    count
}
