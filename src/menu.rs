//! The screen shown when `brahma` is started with no arguments.
//!
//! It collects the settings a person needs and then returns a [`GenerateArgs`]
//! for the same `generate` path as the command line. The progress line is
//! printed after this screen restores the terminal.
use crate::{
    Error, GenerateArgs,
    area::{self, STRIP_WIDTH},
    fast_terrain::GpuChoice,
    level::{self, Quality},
    resources,
};
use horizon_world::compression::Compression;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};
use ratatui::Terminal;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

const BG: Color = Color::Rgb(28, 25, 23);
const PANEL: Color = Color::Rgb(41, 37, 36);
const TEXT: Color = Color::Rgb(250, 250, 249);
const MUTED: Color = Color::Rgb(168, 162, 158);
const AMBER: Color = Color::Rgb(217, 119, 6);
const ROW: Color = Color::Rgb(68, 64, 60);
const EXACT: Color = Color::Rgb(101, 163, 13);
const FAST: Color = Color::Rgb(8, 145, 178);
const DANGER: Color = Color::Rgb(244, 63, 94);

const SERVER_PAGE: &str = "https://www.minecraft.net/en-us/download/server";

static RAN: AtomicBool = AtomicBool::new(false);
static SEED_N: AtomicU64 = AtomicU64::new(1);

/// A generate was started from the menu, so the console should wait before closing.
pub fn mark_ran() {
    RAN.store(true, Ordering::Relaxed);
}

pub fn ran() -> bool {
    RAN.load(Ordering::Relaxed)
}

/// Both streams are a terminal, so the menu can be drawn and keys can be read.
pub fn is_terminal() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

/// One new key, then the shell is usable again. Used after a menu-launched run.
/// Keys already queued (the Enter that started generation) are discarded first.
pub fn pause() {
    eprintln!("Press any key to close.");
    let _ = enable_raw_mode();
    while event::poll(Duration::ZERO).unwrap_or(false) {
        let _ = event::read();
    }
    loop {
        let Ok(Event::Key(key)) = event::read() else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        break;
    }
    let _ = disable_raw_mode();
}

/// `None` when the person quits. `Some` is ready for `pipeline::generate`.
pub fn run() -> Result<Option<GenerateArgs>, Error> {
    let mut form = Form::cli_defaults();
    form.seed_text = random_seed();
    enable_raw_mode()?;
    let _restore = Restore;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    let mut selected = 0usize;
    loop {
        let rows = form.rows();
        if selected >= rows.len() {
            selected = rows.len() - 1;
        }
        terminal.draw(|frame| draw(frame, &form, selected))?;
        if !event::poll(Duration::from_millis(200))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Ok(None);
        }
        match key.code {
            KeyCode::Esc => {
                if !form.cancel_edit() {
                    return Ok(None);
                }
            }
            KeyCode::Up => {
                form.commit_edit();
                selected = selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Tab => {
                form.commit_edit();
                if selected + 1 < rows.len() {
                    selected += 1;
                }
            }
            KeyCode::Left => {
                if !form.editing() {
                    form.cycle(rows[selected], -1);
                }
            }
            KeyCode::Right => {
                if !form.editing() {
                    form.cycle(rows[selected], 1);
                }
            }
            KeyCode::Enter => {
                if form.editing() {
                    form.commit_edit();
                } else if rows[selected] == Row::Server && form.server == ServerChoice::Page {
                    open_page(SERVER_PAGE);
                    let (_, url) = pinned_server();
                    form.status = Some(format!("{SERVER_PAGE}    {url}"));
                    form.error = None;
                } else if rows[selected].editable() {
                    form.begin_edit(rows[selected]);
                } else if rows[selected] == Row::Start {
                    match form.to_args() {
                        Ok(mut args) => match form.attach_jars(&mut terminal, selected) {
                            Ok((minecraft_dir, server_jar)) => {
                                args.minecraft_dir = minecraft_dir;
                                args.server_jar = server_jar;
                                return Ok(Some(args));
                            }
                            Err(error) => form.error = Some(error),
                        },
                        Err(error) => form.error = Some(error),
                    }
                }
            }
            KeyCode::Char('r') | KeyCode::Char('R')
                if !form.editing() && rows[selected] == Row::Seed =>
            {
                form.seed_text = random_seed();
                form.error = None;
            }
            KeyCode::Backspace if form.editing() => {
                form.edit_mut().pop();
                form.error = None;
                form.status = None;
            }
            KeyCode::Char(ch) if form.editing() && !key.modifiers.contains(KeyModifiers::CONTROL) => {
                form.edit_mut().push(ch);
                form.error = None;
                form.status = None;
            }
            _ => {}
        }
    }
}

struct Restore;

impl Drop for Restore {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

fn random_seed() -> String {
    let n = SEED_N.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1);
    (nanos.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ n.wrapping_mul(0x6C62_272E_07BB_0142)).to_string()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Row {
    World,
    Seed,
    Radius,
    Shape,
    Where,
    Quality,
    Gpu,
    Structures,
    Threads,
    Client,
    Server,
    Start,
}

impl Row {
    fn editable(self) -> bool {
        matches!(
            self,
            Self::World | Self::Seed | Self::Radius | Self::Threads | Self::Client | Self::Server
        )
    }

    fn label(self) -> &'static str {
        match self {
            Self::World => "World folder",
            Self::Seed => "Seed",
            Self::Radius => "Radius",
            Self::Shape => "Shape",
            Self::Where => "Dimensions",
            Self::Quality => "Quality",
            Self::Gpu => "GPU",
            Self::Structures => "Structures",
            Self::Threads => "Threads",
            Self::Client => "Client jar",
            Self::Server => "Server jar",
            Self::Start => "Generate",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RadiusPreset {
    Village,
    Normal,
    Large,
    Custom,
}

impl RadiusPreset {
    fn cycle(self, dir: i32) -> Self {
        let all = [Self::Village, Self::Normal, Self::Large, Self::Custom];
        step(&all, self, dir)
    }

    fn blocks(self, custom: &str) -> Result<i32, String> {
        match self {
            Self::Village => Ok(512),
            Self::Normal => Ok(2500),
            Self::Large => Ok(10000),
            Self::Custom => custom
                .trim()
                .parse::<i32>()
                .ok()
                .filter(|n| *n >= 0)
                .ok_or_else(|| "Radius is a number of blocks, zero or more.".to_string()),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Where {
    Overworld,
    All,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ServerChoice {
    Download,
    Path,
    Page,
}

impl ServerChoice {
    fn cycle(self, dir: i32) -> Self {
        let all = [Self::Download, Self::Path, Self::Page];
        step(&all, self, dir)
    }
}

struct Form {
    world: String,
    seed_text: String,
    radius: RadiusPreset,
    custom_radius: String,
    shape: area::Shape,
    where_: Where,
    quality: Quality,
    gpu: GpuChoice,
    /// Saved on the world the same way as `--no-structures`. Fast still shows Off.
    structures: bool,
    threads_auto: bool,
    threads_text: String,
    /// Shown when the asset cache is not prepared yet.
    ask_jars: bool,
    client_text: String,
    server: ServerChoice,
    server_path: String,
    /// Backup of the field being typed, so Esc restores it.
    editing: Option<(Row, String)>,
    error: Option<String>,
    status: Option<String>,
}

impl Form {
    /// Command-line defaults. The seed is a placeholder the menu replaces.
    fn cli_defaults() -> Self {
        let cpus = std::thread::available_parallelism().map_or(2, |n| n.get());
        Self {
            world: "worlds/world".to_string(),
            seed_text: "1".to_string(),
            radius: RadiusPreset::Normal,
            custom_radius: "2500".to_string(),
            shape: area::Shape::Square,
            where_: Where::Overworld,
            quality: Quality::Exact,
            gpu: GpuChoice::Auto,
            structures: true,
            threads_auto: true,
            threads_text: cpus.to_string(),
            ask_jars: crate::cache::existing().is_none(),
            client_text: default_game_dir(),
            server: ServerChoice::Download,
            server_path: String::new(),
            editing: None,
            error: None,
            status: None,
        }
    }

    fn rows(&self) -> Vec<Row> {
        let mut rows = vec![
            Row::World,
            Row::Seed,
            Row::Radius,
            Row::Shape,
            Row::Where,
            Row::Quality,
        ];
        if self.quality == Quality::Fast {
            rows.push(Row::Gpu);
        }
        rows.extend([Row::Structures, Row::Threads]);
        if self.ask_jars {
            rows.extend([Row::Client, Row::Server]);
        }
        rows.push(Row::Start);
        rows
    }

    fn editing(&self) -> bool {
        self.editing.is_some()
    }

    fn begin_edit(&mut self, row: Row) {
        if row == Row::Radius && self.radius != RadiusPreset::Custom {
            return;
        }
        if row == Row::Threads && self.threads_auto {
            return;
        }
        if row == Row::Server && self.server != ServerChoice::Path {
            return;
        }
        let current = match row {
            Row::World => self.world.clone(),
            Row::Seed => self.seed_text.clone(),
            Row::Radius => self.custom_radius.clone(),
            Row::Threads => self.threads_text.clone(),
            Row::Client => self.client_text.clone(),
            Row::Server => self.server_path.clone(),
            _ => return,
        };
        self.editing = Some((row, current));
    }

    fn commit_edit(&mut self) {
        self.editing = None;
    }

    /// Restore the field. Returns whether an edit was in progress.
    fn cancel_edit(&mut self) -> bool {
        let Some((row, backup)) = self.editing.take() else {
            return false;
        };
        match row {
            Row::World => self.world = backup,
            Row::Seed => self.seed_text = backup,
            Row::Radius => self.custom_radius = backup,
            Row::Threads => self.threads_text = backup,
            Row::Client => self.client_text = backup,
            Row::Server => self.server_path = backup,
            _ => {}
        }
        true
    }

    fn edit_mut(&mut self) -> &mut String {
        let row = self.editing.as_ref().map(|(row, _)| *row);
        match row {
            Some(Row::World) => &mut self.world,
            Some(Row::Seed) => &mut self.seed_text,
            Some(Row::Radius) => &mut self.custom_radius,
            Some(Row::Threads) => &mut self.threads_text,
            Some(Row::Client) => &mut self.client_text,
            Some(Row::Server) => &mut self.server_path,
            _ => unreachable!("edit buffer without a text field"),
        }
    }

    fn cycle(&mut self, row: Row, dir: i32) {
        self.error = None;
        self.status = None;
        match row {
            Row::Radius => self.radius = self.radius.cycle(dir),
            Row::Shape => {
                self.shape = match self.shape {
                    area::Shape::Square => area::Shape::Circle,
                    area::Shape::Circle => area::Shape::Square,
                };
            }
            Row::Where => {
                self.where_ = match self.where_ {
                    Where::Overworld => Where::All,
                    Where::All => Where::Overworld,
                };
            }
            Row::Quality => {
                self.quality = match self.quality {
                    Quality::Exact => Quality::Fast,
                    Quality::Fast => Quality::Exact,
                };
            }
            Row::Gpu => {
                self.gpu = match self.gpu {
                    GpuChoice::Auto => {
                        if dir < 0 {
                            GpuChoice::Off
                        } else {
                            GpuChoice::On
                        }
                    }
                    GpuChoice::On => {
                        if dir < 0 {
                            GpuChoice::Auto
                        } else {
                            GpuChoice::Off
                        }
                    }
                    GpuChoice::Off => {
                        if dir < 0 {
                            GpuChoice::On
                        } else {
                            GpuChoice::Auto
                        }
                    }
                };
            }
            Row::Structures if self.quality == Quality::Exact => {
                self.structures = !self.structures;
            }
            Row::Threads => self.threads_auto = !self.threads_auto,
            Row::Server => self.server = self.server.cycle(dir),
            _ => {}
        }
    }

    /// Client install and server jar for the first prepare. Both stay empty
    /// when the asset cache already exists.
    fn attach_jars(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        selected: usize,
    ) -> Result<(Option<PathBuf>, Option<PathBuf>), String> {
        if !self.ask_jars {
            return Ok((None, None));
        }
        let minecraft_dir = game_folder(&self.client_text)?;
        let server_jar = match self.server {
            ServerChoice::Page => {
                return Err(
                    "Choose Download, or set a server jar path. Enter on Server jar opens the page."
                        .into(),
                );
            }
            ServerChoice::Path => {
                let path = PathBuf::from(self.server_path.trim());
                if !path.is_file() {
                    return Err("Set the path to the 26.3 server jar.".into());
                }
                path
            }
            ServerChoice::Download => {
                self.status = Some("Downloading the 26.3 server jar from Mojang…".into());
                self.error = None;
                terminal
                    .draw(|frame| draw(frame, self, selected))
                    .map_err(|e| e.to_string())?;
                let dest = crate::cache::server_jar_download();
                download_server_jar(&dest)?;
                self.status = None;
                dest
            }
        };
        Ok((Some(minecraft_dir), Some(server_jar)))
    }

    fn to_args(&self) -> Result<GenerateArgs, String> {
        let world = self.world.trim();
        if world.is_empty() {
            return Err("Choose a world folder.".into());
        }
        let seed = resources::parse_seed(&self.seed_text)
            .ok_or_else(|| "Enter a seed. It can be a number or a word.".to_string())?;
        let radius = self.radius.blocks(&self.custom_radius)?;
        let threads = if self.threads_auto {
            std::thread::available_parallelism().map_or(2, |n| n.get())
        } else {
            self.threads_text
                .trim()
                .parse::<usize>()
                .ok()
                .filter(|n| *n >= 1)
                .ok_or_else(|| "Threads is a whole number, at least 1.".to_string())?
        };
        let quality = self.quality;
        if let Some(saved) = level::read_run(PathBuf::from(world).as_path())
            .map_err(|e| e.to_string())?
        {
            let structures = self.structures;
            if saved.seed != seed
                || saved.preset != "minecraft:normal"
                || saved.structures != structures
                || saved.quality != quality
            {
                return Err(format!(
                    "This world is seed {} {}, structures {}, {}. The menu is seed {seed}, minecraft:normal, structures {structures}, {}.",
                    saved.seed,
                    saved.preset,
                    saved.structures,
                    saved.quality.name(),
                    quality.name()
                ));
            }
        }
        Ok(GenerateArgs {
            resources: None,
            minecraft_dir: None,
            server_jar: None,
            java_home: None,
            out: PathBuf::from(world),
            seed,
            name: None,
            radius,
            shape: self.shape,
            center: None,
            threads,
            strip_width: STRIP_WIDTH,
            compression: Compression::Zlib,
            quality,
            gpu: self.gpu,
            order_file: None,
            verbose: false,
            debug: false,
            dimensions: Vec::new(),
            all_dimensions: self.where_ == Where::All,
            preset: "minecraft:normal".to_string(),
            structures: self.structures,
            datapacks: None,
        })
    }

    fn notice(&self) -> Line<'_> {
        if let Some(error) = &self.error {
            return Line::from(Span::styled(error.clone(), Style::default().fg(DANGER)));
        }
        if let Some(status) = &self.status {
            return Line::from(Span::styled(status.clone(), Style::default().fg(AMBER)));
        }
        if self.ask_jars && self.server == ServerChoice::Page {
            let (_, url) = pinned_server();
            return Line::from(Span::styled(
                format!("{SERVER_PAGE}    {url}"),
                Style::default().fg(AMBER),
            ));
        }
        match self.preview() {
            Preview::New => Line::from(Span::styled(
                "A new world. The game lights chunks when you load them.",
                Style::default().fg(MUTED),
            )),
            Preview::Resume => Line::from(Span::styled(
                "This folder will continue. Finished chunks stay as they are.",
                Style::default().fg(EXACT),
            )),
            Preview::Problem(text) => Line::from(Span::styled(text, Style::default().fg(DANGER))),
        }
    }

    fn preview(&self) -> Preview {
        let world = self.world.trim();
        if world.is_empty() {
            return Preview::Problem("Choose a world folder.".into());
        }
        if resources::parse_seed(&self.seed_text).is_none() {
            return Preview::Problem("Enter a seed. It can be a number or a word.".into());
        }
        if self.radius.blocks(&self.custom_radius).is_err() {
            return Preview::Problem("Radius is a number of blocks, zero or more.".into());
        }
        match level::read_run(PathBuf::from(world).as_path()) {
            Err(error) => Preview::Problem(error.to_string()),
            Ok(None) => Preview::New,
            Ok(Some(_)) => match self.to_args() {
                Ok(_) => Preview::Resume,
                Err(error) => Preview::Problem(error),
            },
        }
    }
}

enum Preview {
    New,
    Resume,
    Problem(String),
}

fn step<T: Copy + PartialEq>(all: &[T], current: T, dir: i32) -> T {
    let i = all.iter().position(|v| *v == current).unwrap_or(0);
    let n = all.len() as i32;
    let next = (i as i32 + dir).rem_euclid(n) as usize;
    all[next]
}

fn draw(frame: &mut ratatui::Frame, form: &Form, selected: usize) {
    let area = frame.size();
    frame.render_widget(Block::default().style(Style::default().bg(BG)), area);
    let rows = form.rows();
    let height = (rows.len() as u16).saturating_add(10).min(area.height);
    let width = 76.min(area.width);
    let panel = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    let mut lines = vec![
        Line::from(Span::styled(
            "Java Edition 26.3",
            Style::default().fg(MUTED),
        )),
        Line::from(""),
    ];
    for (i, row) in rows.iter().copied().enumerate() {
        lines.push(row_line(form, row, i == selected));
        if row == Row::Quality {
            lines.push(Line::from(Span::styled(
                format!("  {}", quality_hint(form.quality)),
                Style::default().fg(if form.quality == Quality::Exact {
                    EXACT
                } else {
                    FAST
                }),
            )));
        }
    }
    lines.push(Line::from(""));
    lines.push(form.notice());
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "↑↓ move    ←→ change    enter edit or start    r new seed    esc quit",
        Style::default().fg(MUTED),
    )));
    let block = Block::default()
        .title(Span::styled(
            " Brahma ",
            Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(AMBER))
        .style(Style::default().bg(PANEL).fg(TEXT));
    frame.render_widget(
        Paragraph::new(lines).block(block).wrap(Wrap { trim: false }),
        panel,
    );
}

fn row_line(form: &Form, row: Row, selected: bool) -> Line<'static> {
    let label = format!("{:<14}", row.label());
    let value = display_value(form, row);
    let value_color = match row {
        Row::Quality if form.quality == Quality::Exact => EXACT,
        Row::Quality => FAST,
        Row::Start => BG,
        _ => TEXT,
    };
    if row == Row::Start {
        let style = if selected {
            Style::default().fg(BG).bg(AMBER).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(AMBER)
        };
        return Line::from(Span::styled(format!("  {value} "), style));
    }
    let bg = if selected { ROW } else { PANEL };
    let mark = if selected { "› " } else { "  " };
    Line::from(vec![
        Span::styled(mark, Style::default().fg(AMBER).bg(bg)),
        Span::styled(label, Style::default().fg(MUTED).bg(bg)),
        Span::styled(value, Style::default().fg(value_color).bg(bg)),
    ])
}

fn display_value(form: &Form, row: Row) -> String {
    let editing = form.editing.as_ref().is_some_and(|(r, _)| *r == row);
    let typed = |text: &str| {
        if editing {
            format!("{text}▌")
        } else {
            text.to_string()
        }
    };
    match row {
        Row::World => typed(&form.world),
        Row::Seed => typed(&form.seed_text),
        Row::Radius => match form.radius {
            RadiusPreset::Village => "Village  ·  512 blocks".into(),
            RadiusPreset::Normal => "Normal  ·  2,500 blocks".into(),
            RadiusPreset::Large => "Large  ·  10,000 blocks".into(),
            RadiusPreset::Custom => format!("Custom  ·  {} blocks", typed(&form.custom_radius)),
        },
        Row::Shape => match form.shape {
            area::Shape::Square => "Square".into(),
            area::Shape::Circle => "Circle".into(),
        },
        Row::Where => match form.where_ {
            Where::Overworld => "Overworld".into(),
            Where::All => "Overworld, Nether, and End".into(),
        },
        Row::Quality => match form.quality {
            Quality::Exact => "Exact".into(),
            Quality::Fast => "Fast".into(),
        },
        Row::Gpu => match form.gpu {
            GpuChoice::Auto => "Auto".into(),
            GpuChoice::On => "On".into(),
            GpuChoice::Off => "Off".into(),
        },
        Row::Structures => {
            if form.quality == Quality::Fast {
                "Off".into()
            } else if form.structures {
                "On".into()
            } else {
                "Off".into()
            }
        }
        Row::Threads => {
            if form.threads_auto {
                format!("Auto  ·  {}", form.threads_text)
            } else {
                typed(&form.threads_text)
            }
        }
        Row::Client => {
            let mark = if client_jar(&game_folder_loose(&form.client_text)).is_file() {
                "found"
            } else {
                "not found"
            };
            format!("{}  ·  {mark}", typed(&form.client_text))
        }
        Row::Server => match form.server {
            ServerChoice::Download => "Download from Mojang".into(),
            ServerChoice::Path => {
                if form.server_path.trim().is_empty() && !editing {
                    "Path  ·  enter the server.jar location".into()
                } else {
                    format!("Path  ·  {}", typed(&form.server_path))
                }
            }
            ServerChoice::Page => "Show download page".into(),
        },
        Row::Start => "Generate".into(),
    }
}

fn default_game_dir() -> String {
    if cfg!(target_os = "macos") {
        return home().join("Library/Application Support/minecraft").display().to_string();
    }
    if cfg!(windows) {
        return std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(home)
            .join(".minecraft")
            .display()
            .to_string();
    }
    home().join(".minecraft").display().to_string()
}

fn home() -> PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn client_jar(dir: &Path) -> PathBuf {
    let version = horizon_core::version::VANILLA_VERSION;
    dir.join("versions")
        .join(version)
        .join(format!("{version}.jar"))
}

/// A typed folder, or the game folder that holds `versions/<version>/<version>.jar`.
fn game_folder(text: &str) -> Result<PathBuf, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err(
            "Set the Minecraft folder, or the path to versions/26.3/26.3.jar.".into(),
        );
    }
    let path = PathBuf::from(text);
    let dir = if path.is_dir() {
        path
    } else if let Some(dir) = game_dir_of_client_jar(&path) {
        dir
    } else {
        return Err(
            "Point at the Minecraft folder, or at versions/26.3/26.3.jar inside it.".into(),
        );
    };
    let jar = client_jar(&dir);
    if !jar.is_file() {
        return Err(format!("Client jar not found at {}", jar.display()));
    }
    Ok(dir)
}

fn game_folder_loose(text: &str) -> PathBuf {
    let path = PathBuf::from(text.trim());
    if path.is_dir() {
        path
    } else {
        game_dir_of_client_jar(&path).unwrap_or(path)
    }
}

fn game_dir_of_client_jar(path: &Path) -> Option<PathBuf> {
    let version = horizon_core::version::VANILLA_VERSION;
    let name = format!("{version}.jar");
    if path.file_name()?.to_str()? != name {
        return None;
    }
    let version_dir = path.parent()?;
    let versions = version_dir.parent()?;
    if versions.file_name()?.to_str()? != "versions" {
        return None;
    }
    versions.parent().map(Path::to_path_buf)
}

fn pinned_server() -> (String, String) {
    let fallback = "33680f5f2ac32864d6d7cf5e56a705fdb3e05f4c";
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("prepare/research-lock.json");
    let sha = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|value| {
            value["minecraft"]["server_sha1"]
                .as_str()
                .map(str::to_string)
        })
        .filter(|sha| !sha.is_empty())
        .unwrap_or_else(|| fallback.to_string());
    let url = format!("https://piston-data.mojang.com/v1/objects/{sha}/server.jar");
    (sha, url)
}

fn download_server_jar(dest: &Path) -> Result<(), String> {
    let (sha, url) = pinned_server();
    if dest.is_file() && sha1_file(dest).ok().as_deref() == Some(sha.as_str()) {
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let status = Command::new("curl")
        .args(["-fL", "--retry", "2", "-o"])
        .arg(dest)
        .arg(&url)
        .status()
        .map_err(|e| {
            format!("could not run curl ({e}). Set a server jar path, or open {SERVER_PAGE}")
        })?;
    if !status.success() {
        let _ = std::fs::remove_file(dest);
        return Err(format!(
            "Download failed. Save the jar from {SERVER_PAGE} and set its path."
        ));
    }
    let got = sha1_file(dest)?;
    if got != sha {
        let _ = std::fs::remove_file(dest);
        return Err(format!(
            "Downloaded jar is not the 26.3 server (sha1 {got})."
        ));
    }
    Ok(())
}

fn sha1_file(path: &Path) -> Result<String, String> {
    use sha1::{Digest, Sha1};
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Sha1::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn open_page(url: &str) {
    let mut command = if cfg!(windows) {
        let mut command = Command::new("cmd");
        command.args(["/C", "start", "", url]);
        command
    } else if cfg!(target_os = "macos") {
        let mut command = Command::new("open");
        command.arg(url);
        command
    } else {
        let mut command = Command::new("xdg-open");
        command.arg(url);
        command
    };
    let _ = command.spawn();
}

fn quality_hint(quality: Quality) -> &'static str {
    match quality {
        Quality::Exact => "Same terrain as the game, including structures.",
        Quality::Fast => "Quicker playable terrain. Hills do not match the seed. No structures.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_form_matches_the_command_line() {
        let mut form = Form::cli_defaults();
        form.world = format!(
            "brahma-menu-absent-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let args = form.to_args().unwrap();
        assert_eq!(args.radius, 2500);
        assert_eq!(args.shape, area::Shape::Square);
        assert_eq!(args.quality, Quality::Exact);
        assert_eq!(args.gpu, GpuChoice::Auto);
        assert!(args.dimensions.is_empty());
        assert!(!args.all_dimensions);
        assert!(args.structures);
        let threads = std::thread::available_parallelism().map_or(2, |n| n.get());
        assert_eq!(args.threads, threads);
        assert_eq!(args.preset, "minecraft:normal");
        assert_eq!(args.compression, Compression::Zlib);
        assert_eq!(args.strip_width, STRIP_WIDTH);
        assert!(args.center.is_none());
        assert_eq!(args.seed, 1);
    }

    #[test]
    fn a_client_jar_path_points_at_its_game_folder() {
        let version = horizon_core::version::VANILLA_VERSION;
        let jar = PathBuf::from(format!("game/versions/{version}/{version}.jar"));
        assert_eq!(
            game_dir_of_client_jar(&jar).unwrap(),
            PathBuf::from("game")
        );
        assert!(game_dir_of_client_jar(Path::new("loose/client.jar")).is_none());
    }

    #[test]
    fn the_server_download_is_the_pinned_jar() {
        let (sha, url) = pinned_server();
        assert!(url.contains(&sha));
        assert!(url.starts_with("https://piston-data.mojang.com/v1/objects/"));
        assert!(url.ends_with("/server.jar"));
    }
}
