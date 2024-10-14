use crossterm::{
    cursor::{Hide, MoveTo, MoveToNextLine, Show},
    event::{self, KeyCode},
    execute,
    style::{Print, PrintStyledContent, ResetColor, Stylize},
    terminal::{disable_raw_mode, enable_raw_mode, Clear, ClearType},
};
use std::io::{stdout, Write};

use crate::dependency::{Dependencies, Dependency};

pub struct State {
    stdout: std::io::Stdout,
    selected: Vec<bool>,
    cursor_location: usize,
    outdated_deps: Dependencies,
    total_deps: usize,
    longest_attributes: Longest,
}

pub enum Event {
    HandleKeyboardEvent,
    UpdateDependencies,
    Exit,
}

struct Longest {
    name: usize,
    current_version: usize,
    latest_version: usize,
}

impl Longest {
    fn get_longest_attributes(dependencies: &Dependencies) -> Longest {
        let mut name = 0;
        let mut current_version = 0;
        let mut latest_version = 0;

        for dep in dependencies.iter() {
            name = name.max(dep.name.len());
            current_version = current_version.max(dep.current_version.len());
            latest_version = latest_version.max(dep.latest_version.len());
        }

        Longest {
            name,
            current_version,
            latest_version,
        }
    }
}

impl State {
    pub fn new(outdated_deps: Dependencies, total_deps: usize) -> Self {
        Self {
            stdout: stdout(),
            selected: vec![false; outdated_deps.len()],
            cursor_location: 0,
            longest_attributes: Longest::get_longest_attributes(&outdated_deps),
            outdated_deps,
            total_deps,
        }
    }

    pub fn start(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        enable_raw_mode()?;
        execute!(self.stdout, Hide)?;
        Ok(())
    }

    pub fn handle_keyboard_event(&mut self) -> Result<Event, Box<dyn std::error::Error>> {
        if let event::Event::Key(key) = event::read()? {
            match key.code {
                KeyCode::Up | KeyCode::Left => {
                    self.cursor_location = if self.cursor_location == 0 {
                        self.outdated_deps.len() - 1
                    } else {
                        self.cursor_location - 1
                    };
                }
                KeyCode::Down | KeyCode::Right => {
                    self.cursor_location = (self.cursor_location + 1) % self.outdated_deps.len();
                }
                KeyCode::Char(' ') => {
                    self.selected[self.cursor_location] = !self.selected[self.cursor_location];
                }
                KeyCode::Enter => {
                    execute!(self.stdout, Show, ResetColor)?;
                    disable_raw_mode()?;
                    return Ok(Event::UpdateDependencies);
                }
                KeyCode::Char('a') => {
                    self.selected = vec![true; self.outdated_deps.len()];
                }
                KeyCode::Char('i') => {
                    self.selected = self.selected.iter().map(|s| !s).collect();
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    execute!(self.stdout, Show, ResetColor)?;
                    disable_raw_mode()?;
                    return Ok(Event::Exit);
                }
                _ => {}
            }
        }

        Ok(Event::HandleKeyboardEvent)
    }

    pub fn selected_dependencies(self) -> Dependencies {
        Dependencies::new(
            self.outdated_deps
                .into_iter()
                .zip(self.selected.iter())
                .filter(|(_, s)| **s)
                .map(|(d, _)| d)
                .collect(),
        )
    }

    pub fn render(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.render_header()?;
        self.render_dependencies()?;
        self.render_footer_actions()?;

        self.stdout.flush()?;
        Ok(())
    }

    fn render_header(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        execute!(
            self.stdout,
            Clear(ClearType::All),
            MoveTo(0, 0),
            Print(format!(
                "{} out of the {} direct dependencies are outdated",
                self.outdated_deps.len().to_string().bold(),
                self.total_deps.to_string().bold()
            )),
            MoveToNextLine(2)
        )?;
        Ok(())
    }

    fn render_dependencies(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        execute!(
            self.stdout,
            PrintStyledContent(
                format!(
                    "Dependencies ({} selected):",
                    self.selected.iter().filter(|s| **s).count()
                )
                .cyan()
            ),
            MoveToNextLine(1)
        )?;

        for (i, dependency) in self.outdated_deps.clone().iter().enumerate() {
            self.render_dependency(i, &dependency)?;
        }

        if self.outdated_deps.is_empty() {
            execute!(
                self.stdout,
                PrintStyledContent("No dependencies found".dim()),
                MoveToNextLine(1),
            )?;
        }

        Ok(())
    }

    fn render_footer_actions(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        execute!(
            self.stdout,
            MoveToNextLine(2),
            Print(format!(
                "Use {} to navigate, {} to select all, {} to invert, {} to select/deselect, {} to update, {}/{} to exit",
                "arrow keys".cyan(),
                "<a>".cyan(),
                "<i>".cyan(),
                "<space>".cyan(),
                "<enter>".cyan(),
                "<esc>".cyan(), "<q>".cyan()
            ))
        )?;
        Ok(())
    }

    fn render_dependency(
        &mut self,
        i: usize,
        Dependency {
            name,
            current_version,
            latest_version,
            repository,
            description,
            latest_version_date,
            current_version_date,
            ..
        }: &Dependency,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let name_spacing = " ".repeat(self.longest_attributes.name - name.len());
        let current_version_spacing =
            " ".repeat(self.longest_attributes.current_version - current_version.len());
        let latest_version_spacing =
            " ".repeat(self.longest_attributes.latest_version - latest_version.len());

        let bullet = if self.selected[i] { "●" } else { "○" };

        let latest_version_date = get_date_from_datetime_string(latest_version_date.as_deref())
            .unwrap_or("none")
            .italic()
            .dim();
        let current_version_date = get_date_from_datetime_string(current_version_date.as_deref())
            .unwrap_or("none")
            .italic()
            .dim();

        let name = name.clone().bold();
        let repository = repository.as_deref().unwrap_or("none").underline_black();
        let description = description
            .as_deref()
            .unwrap_or("")
            .chars()
            .take(60)
            .collect::<String>()
            .dim();

        let row = format!(
            "{bullet} {name}{name_spacing}  {current_version_date} {current_version}{current_version_spacing} -> {latest_version_date} {latest_version}{latest_version_spacing}  {repository} - {description}",
        );

        let colored_row = if i == self.cursor_location {
            row.green()
        } else {
            row.black()
        };

        execute!(
            self.stdout,
            PrintStyledContent(colored_row),
            MoveToNextLine(1),
        )?;
        Ok(())
    }
}

fn get_date_from_datetime_string(datetime_string: Option<&str>) -> Option<&str> {
    datetime_string
        .and_then(|s| s.split_once('T'))
        .map(|(date, _)| date)
}
