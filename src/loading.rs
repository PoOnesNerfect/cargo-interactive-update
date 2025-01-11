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
}

pub fn init_loader(total_deps: usize) -> Result<Loader, Box<dyn std::error::Error>> {
    let mut state = LoaderInner {
        total_deps,
        loaded_deps: 0,
        stdout: stdout(),
    };

    enable_raw_mode()?;
    queue!(state.stdout, Hide, Clear(ClearType::All))?;
    queue!(
        state.stdout,
        MoveTo(0, 0),
        Print("Scanning dependencies"),
        MoveToNextLine(1)
    )?;
    queue!(
        state.stdout,
        Print(format!("[{}] 0/{} 0%", "-".repeat(total_deps), total_deps))
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
        } = &mut state.deref_mut();

        *loaded_deps += 1;

        let perc = (100. * *loaded_deps as f32 / *total_deps as f32) as u8;

        queue!(
            stdout,
            MoveToColumn(*loaded_deps as u16),
            Print("="),
            MoveToColumn(3 + *total_deps as u16),
            Print(format!("{loaded_deps}/{total_deps} {perc}%",))
        )
        .unwrap();

        stdout.flush().unwrap();
    }
}
