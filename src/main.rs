mod layout;
mod scan;

use crate::layout::{grid_layout, treemap, BlockRect};
use crate::scan::{start_scan, Item, ItemKind, ScanHandle, ScanMsg, ViewMode};
use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, MouseEventKind};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Terminal;
use std::collections::HashMap;
use std::ffi::CString;
use std::env;
use std::io::{self, Stdout};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const VERSION_LABEL: &str = concat!("v", env!("CARGO_PKG_VERSION"));

#[derive(Default)]
struct ScanState {
    scanning: bool,
    scanned: u64,
    errors: u64,
}

struct ClickTarget {
    rect: Rect,
    index: usize,
}

struct ConfirmAction {
    target_path: PathBuf,
    target_name: String,
    is_dir: bool,
    return_path: Option<PathBuf>,
}

struct App {
    current_path: PathBuf,
    items: Vec<Item>,
    total: u64,
    layout_sizes: Vec<(usize, u64)>,
    layout_has_zero: bool,
    scan_state: ScanState,
    scan_handle: Option<ScanHandle>,
    view_mode: ViewMode,
    click_map: Vec<ClickTarget>,
    up_rect: Option<Rect>,
    spinner: usize,
    last_error: Option<String>,
    fs_used: u64,
    fs_total: u64,
    fs_last: Instant,
    fs_device: Option<String>,
    scan_cache: HashMap<CacheKey, CachedScan>,
    confirm: Option<ConfirmAction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    path: PathBuf,
    view: ViewMode,
}

#[derive(Debug, Clone)]
struct CachedScan {
    items: Vec<Item>,
    total: u64,
    layout_sizes: Vec<(usize, u64)>,
    layout_has_zero: bool,
    errors: u64,
}

impl App {
    fn new(path: PathBuf) -> Self {
        Self {
            current_path: path,
            items: Vec::new(),
            total: 0,
            layout_sizes: Vec::new(),
            layout_has_zero: false,
            scan_state: ScanState::default(),
            scan_handle: None,
            view_mode: ViewMode::Dirs,
            click_map: Vec::new(),
            up_rect: None,
            spinner: 0,
            last_error: None,
            fs_used: 0,
            fs_total: 0,
            fs_last: Instant::now() - Duration::from_secs(10),
            fs_device: None,
            scan_cache: HashMap::new(),
            confirm: None,
        }
    }

    fn start_scan(&mut self) {
        if let Some(handle) = &self.scan_handle {
            handle.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        let key = CacheKey {
            path: self.current_path.clone(),
            view: self.view_mode,
        };
        if let Some(cached) = self.scan_cache.get(&key).cloned() {
            self.items = cached.items;
            self.total = cached.total;
            self.layout_sizes = cached.layout_sizes;
            self.layout_has_zero = cached.layout_has_zero;
            self.scan_state = ScanState {
                scanning: false,
                scanned: self.items.len() as u64,
                errors: cached.errors,
            };
            self.last_error = None;
            self.scan_handle = None;
            return;
        }

        self.items.clear();
        self.total = 0;
        self.layout_sizes.clear();
        self.layout_has_zero = false;
        self.scan_state = ScanState {
            scanning: true,
            scanned: 0,
            errors: 0,
        };
        self.last_error = None;
        self.scan_handle = Some(start_scan(self.current_path.clone(), self.view_mode));
    }

    fn invalidate_cache_for(&mut self, path: &Path) {
        let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.scan_cache
            .retain(|k, _| !k.path.starts_with(&target) && !target.starts_with(&k.path));
    }

    fn go_up(&mut self) {
        if self.view_mode == ViewMode::Files {
            self.view_mode = ViewMode::Dirs;
            self.start_scan();
            return;
        }
        if let Some(parent) = self.current_path.parent().map(Path::to_path_buf) {
            self.current_path = parent;
            self.start_scan();
        }
    }

    fn update_scan(&mut self) -> bool {
        let mut changed = false;
        if let Some(handle) = &self.scan_handle {
            loop {
                match handle.rx.try_recv() {
                    Ok(msg) => match msg {
                        ScanMsg::Progress { scanned, errors } => {
                            self.scan_state.scanned = scanned;
                            self.scan_state.errors = errors;
                            changed = true;
                        }
                        ScanMsg::Done { items, total, errors } => {
                            self.items = items;
                            self.total = total;
                            self.layout_sizes = self
                                .items
                                .iter()
                                .enumerate()
                                .map(|(i, item)| (i, item.size))
                                .collect();
                            self.layout_has_zero = self
                                .items
                                .iter()
                                .any(|i| i.size == 0 && i.kind == ItemKind::Dir);
                            let key = CacheKey {
                                path: self.current_path.clone(),
                                view: self.view_mode,
                            };
                            let cached = CachedScan {
                                items: self.items.clone(),
                                total: self.total,
                                layout_sizes: self.layout_sizes.clone(),
                                layout_has_zero: self.layout_has_zero,
                                errors,
                            };
                            self.scan_cache.insert(key, cached);
                            self.scan_state.scanned = self.items.len() as u64;
                            self.scan_state.errors = errors;
                            self.scan_state.scanning = false;
                            changed = true;
                        }
                        ScanMsg::Error(err) => {
                            self.last_error = Some(err);
                            self.scan_state.scanning = false;
                            changed = true;
                        }
                    },
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        self.scan_state.scanning = false;
                        changed = true;
                        break;
                    }
                }
            }
        }
        changed
    }

    fn update_fs_cache(&mut self) {
        if self.fs_last.elapsed() < Duration::from_secs(1) {
            return;
        }
        if let Some((used, total)) = fs_usage(&self.current_path) {
            self.fs_used = used;
            self.fs_total = total;
        }
        self.fs_device = current_device(&self.current_path);
        self.fs_last = Instant::now();
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let start_path = env::args().nth(1).unwrap_or_else(|| ".".to_string());
    let start_path = PathBuf::from(start_path);

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = run_app(&mut terminal, start_path);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;

    Ok(res?)
}

fn run_app(terminal: &mut Terminal<CrosstermBackend<Stdout>>, start_path: PathBuf) -> io::Result<()> {
    let start_path = fs::canonicalize(&start_path).unwrap_or(start_path);
    let mut app = App::new(start_path);
    app.start_scan();
    app.update_fs_cache();
    terminal.draw(|f| ui(f, &mut app))?;

    let mut last_frame = Instant::now();
    loop {
        let mut dirty = app.update_scan();

        if app.scan_state.scanning && last_frame.elapsed() >= Duration::from_millis(200) {
            app.spinner = (app.spinner + 1) % 4;
            dirty = true;
        }

        if event::poll(Duration::from_millis(200))? {
            dirty = true;
            match event::read()? {
                Event::Key(key) => {
                    if key.kind == KeyEventKind::Press {
                        if app.confirm.is_some() {
                            match key.code {
                                KeyCode::Char('y') | KeyCode::Enter => {
                                    let action = app.confirm.take().unwrap();
                                    if let Err(err) = perform_delete(&action) {
                                        app.last_error = Some(err);
                                    }
                                    app.invalidate_cache_for(&action.target_path);
                                    if let Some(parent) = action.return_path {
                                        app.current_path = parent;
                                        app.view_mode = ViewMode::Dirs;
                                    }
                                    app.start_scan();
                                }
                                KeyCode::Char('n') | KeyCode::Esc => {
                                    app.confirm = None;
                                }
                                _ => {}
                            }
                            continue;
                        }
                        match key.code {
                            KeyCode::Char('q') => break,
                            KeyCode::Backspace | KeyCode::Char('h') | KeyCode::Up | KeyCode::Left | KeyCode::Esc => {
                                app.go_up()
                            }
                            KeyCode::Char('f') => {
                                app.view_mode = if app.view_mode == ViewMode::Dirs {
                                    ViewMode::Files
                                } else {
                                    ViewMode::Dirs
                                };
                                app.start_scan();
                            }
                            KeyCode::Delete => {
                                if let Some(parent) = app.current_path.parent().map(Path::to_path_buf) {
                                    let name = app
                                        .current_path
                                        .file_name()
                                        .unwrap_or_default()
                                        .to_string_lossy()
                                        .to_string();
                                    app.confirm = Some(ConfirmAction {
                                        target_path: app.current_path.clone(),
                                        target_name: name,
                                        is_dir: true,
                                        return_path: Some(parent),
                                    });
                                } else {
                                    app.last_error = Some("Refusing to delete root directory".to_string());
                                }
                            }
                            _ => {}
                        }
                    }
                }
                Event::Mouse(mouse) => {
                    if let MouseEventKind::Down(_) = mouse.kind {
                        let x = mouse.column;
                        let y = mouse.row;

                        if app.confirm.is_some() {
                            continue;
                        }

                        if let Some(up_rect) = app.up_rect {
                            if contains(up_rect, x, y) {
                                app.go_up();
                                continue;
                            }
                        }

                        if let Some(target) = app.click_map.iter().find(|t| contains(t.rect, x, y)) {
                            if let Some(item) = app.items.get(target.index) {
                                if let MouseEventKind::Down(crossterm::event::MouseButton::Right) = mouse.kind {
                                    app.confirm = Some(ConfirmAction {
                                        target_path: item.path.clone(),
                                        target_name: item.name.clone(),
                                        is_dir: item.kind != ItemKind::File,
                                        return_path: None,
                                    });
                                } else {
                                    match item.kind {
                                        ItemKind::Dir => {
                                            app.current_path = item.path.clone();
                                            app.view_mode = ViewMode::Dirs;
                                            app.start_scan();
                                        }
                                        ItemKind::FilesAggregate => {
                                            app.view_mode = ViewMode::Files;
                                            app.start_scan();
                                        }
                                        ItemKind::File => {}
                                    }
                                }
                            }
                        }
                    }
                }
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
        if dirty {
            app.update_fs_cache();
            terminal.draw(|f| ui(f, &mut app))?;
            last_frame = Instant::now();
        }
    }

    Ok(())
}

fn ui(f: &mut ratatui::Frame, app: &mut App) {
    let size = f.size();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(size);

    let main = chunks[0];
    let bottom = chunks[1];

    render_treemap(f, app, main);
    render_bottom(f, app, bottom);
}

fn render_treemap(f: &mut ratatui::Frame, app: &mut App, area: Rect) {
    app.click_map.clear();

    if area.width < 2 || area.height < 2 {
        return;
    }

    f.render_widget(Clear, area);

    if app.scan_state.scanning && app.items.is_empty() {
        let spinner = match app.spinner {
            0 => "|",
            1 => "/",
            2 => "-",
            _ => "\\",
        };
        let msg = format!("Scanning {}  items={} errors={}", spinner, app.scan_state.scanned, app.scan_state.errors);
        let p = Paragraph::new(msg).style(Style::default().fg(Color::Yellow));
        f.render_widget(Clear, area);
        f.render_widget(p, area);
        return;
    }

    if app.items.is_empty() {
        let msg = if let Some(err) = &app.last_error {
            format!("Error: {}", err)
        } else {
            "Empty directory".to_string()
        };
        let p = Paragraph::new(msg).style(Style::default().fg(Color::Yellow));
        f.render_widget(Clear, area);
        f.render_widget(p, area);
        return;
    }

    let sizes = &app.layout_sizes;
    let has_zero = app.layout_has_zero;

    let mut blocks = Vec::new();
    if app.view_mode == ViewMode::Files {
        blocks = grid_layout(sizes, area);
    } else {
        if has_zero {
            blocks = grid_layout(sizes, area);
        } else {
        if let Some((files_idx, files_size, files_count)) = app
            .items
            .iter()
            .enumerate()
            .find(|(_, item)| item.kind == ItemKind::FilesAggregate)
            .map(|(i, item)| (i, item.size, item.count))
        {
            if area.height >= 2 && files_count > 0 {
                let mut files_h = if app.total == 0 {
                    1
                } else {
                    ((area.height as f64) * (files_size as f64 / app.total as f64)).round() as u16
                };
                if files_h == 0 {
                    files_h = 1;
                }
                let top_sizes: Vec<(usize, u64)> =
                    sizes.iter().cloned().filter(|(i, _)| *i != files_idx).collect();
                if !top_sizes.is_empty() && files_h >= area.height {
                    files_h = area.height.saturating_sub(1);
                }
                let top_h = area.height.saturating_sub(files_h);
                if top_h > 0 {
                    let top_area = Rect {
                        x: area.x,
                        y: area.y,
                        width: area.width,
                        height: top_h,
                    };
                    blocks.extend(treemap(&top_sizes, top_area));
                }

                let files_rect = Rect {
                    x: area.x,
                    y: area.y + area.height.saturating_sub(files_h),
                    width: area.width,
                    height: files_h,
                };
                blocks.push(BlockRect {
                    index: files_idx,
                    rect: files_rect,
                });
            } else {
                blocks = treemap(sizes, area);
            }
        } else {
            blocks = treemap(sizes, area);
        }
        if blocks.len() < sizes.len() {
            blocks = grid_layout(sizes, area);
        }
        }
    }
    for block in blocks {
        if block.rect.width < 1 || block.rect.height < 1 {
            continue;
        }
        draw_block(f, app, &block);
        app.click_map.push(ClickTarget {
            rect: block.rect,
            index: block.index,
        });
    }

    if app.scan_state.scanning {
        let spinner = match app.spinner {
            0 => "|",
            1 => "/",
            2 => "-",
            _ => "\\",
        };
        let msg = format!("Scanning {}  items={} errors={}", spinner, app.scan_state.scanned, app.scan_state.errors);
        let overlay = Paragraph::new(msg)
            .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));
        let overlay_area = centered_rect(40, 3, area);
        f.render_widget(Clear, overlay_area);
        f.render_widget(overlay, overlay_area);
    }

    if let Some(confirm) = &app.confirm {
        let msg = format!(
            "Delete {} {}?\n\n[y]es / [n]o",
            if confirm.is_dir { "directory" } else { "file" },
            confirm.target_name
        );
        let overlay = Paragraph::new(msg)
            .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
            .block(Block::default().style(Style::default().bg(Color::Black)));
        let overlay_area = centered_rect(60, 5, area);
        f.render_widget(Clear, overlay_area);
        f.render_widget(overlay, overlay_area);
    }
}

fn draw_block(f: &mut ratatui::Frame, app: &App, block: &BlockRect) {
    let item = &app.items[block.index];
    let color = color_for_item(block.index, item.kind);
    let fg = text_color(color);
    let base_style = Style::default().bg(color).fg(fg);

    let size_text = format_size(item.size);
    let label = label_for_rect(item.name.as_str(), &size_text, block.rect);
    if let Some(label) = label {
        let p = Paragraph::new(label).style(base_style).block(Block::default().style(base_style));
        f.render_widget(p, block.rect);
    } else {
        let b = Block::default().style(base_style);
        f.render_widget(b, block.rect);
    }
}

fn render_bottom(f: &mut ratatui::Frame, app: &mut App, area: Rect) {
    let device_label = app.fs_device.as_deref().unwrap_or("-");
    let version_label = VERSION_LABEL;
    let desired_bar = 20usize;
    let min_bar = 10usize;
    let device_w = device_label.len();
    let version_w = version_label.len();
    let total_w = area.width as usize;

    let info_width = if total_w >= device_w + desired_bar + version_w {
        device_w + desired_bar + version_w
    } else if total_w >= device_w + min_bar + version_w {
        total_w
    } else if total_w >= version_w + min_bar {
        total_w
    } else {
        total_w
    };
    let chunks: Vec<Rect> = if info_width > 0 {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(1), Constraint::Length(info_width as u16)])
            .split(area)
            .to_vec()
    } else {
        vec![area]
    };
    let text_area = chunks[0];

    let up_enabled = app.current_path.parent().is_some();
    let up_label = "[Up]";
    let view_label = match app.view_mode {
        ViewMode::Dirs => "[Dirs]",
        ViewMode::Files => "[Files]",
    };
    let help = "q quit, click to enter, Backspace/h up, f view";

    let mut path = app.current_path.to_string_lossy().to_string();

    let reserved = up_label.len() + 2 + view_label.len() + 2 + help.len() + 2;
    let max_width = text_area.width as usize;
    if max_width > reserved {
        let max_path = max_width - reserved;
        path = truncate_middle(&path, max_path);
    } else if max_width > 3 {
        path = truncate_middle(&path, max_width.saturating_sub(1));
    }

    let mut spans = Vec::new();
    spans.push(Span::styled(path.clone(), Style::default().fg(Color::White)));
    spans.push(Span::raw("  "));

    let up_style = if up_enabled {
        Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    spans.push(Span::styled(up_label, up_style));
    spans.push(Span::raw("  "));
    spans.push(Span::styled(view_label, Style::default().fg(Color::Magenta)));
    spans.push(Span::raw("  "));
    spans.push(Span::styled(help, Style::default().fg(Color::DarkGray)));

    let p = Paragraph::new(Line::from(spans));
    f.render_widget(p, text_area);

    let up_width = up_label.len() as u16;
    let up_x = text_area.x + path.len() as u16 + 2;
    app.up_rect = if up_enabled && up_x + up_width <= text_area.x + text_area.width {
        Some(Rect { x: up_x, y: text_area.y, width: up_width, height: 1 })
    } else {
        None
    };

    if info_width > 0 && chunks.len() > 1 && app.fs_total > 0 {
        render_usage_bar(f, chunks[1], app.fs_used, app.fs_total, device_label, version_label);
    }
}

fn contains(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
}

fn truncate_middle(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    if max <= 3 {
        return "...".to_string();
    }
    let keep = (max - 3) / 2;
    let start = &s[..keep];
    let end = &s[s.len() - keep..];
    format!("{}...{}", start, end)
}

fn label_for_rect(name: &str, size: &str, rect: Rect) -> Option<String> {
    if rect.height < 1 || rect.width < 4 {
        return None;
    }
    let max = rect.width as usize;
    let size_len = size.chars().count();
    if size_len + 1 >= max {
        return None;
    }

    let mut name_max = max - size_len - 1;
    if name_max < 3 {
        return None;
    }

    let name_len = name.chars().count();
    let name_out = if name_len <= name_max {
        name.to_string()
    } else {
        name_max = name_max.saturating_sub(3);
        if name_max == 0 {
            return None;
        }
        let mut out = String::new();
        for (i, ch) in name.chars().enumerate() {
            if i >= name_max {
                break;
            }
            out.push(ch);
        }
        out.push_str("...");
        out
    };

    Some(format!("{} {}", name_out, size))
}

fn color_for_item(idx: usize, kind: ItemKind) -> Color {
    const DIR_COLORS: [Color; 8] = [
        Color::Blue,
        Color::Cyan,
        Color::Green,
        Color::Yellow,
        Color::Magenta,
        Color::LightBlue,
        Color::LightGreen,
        Color::LightYellow,
    ];
    const FILE_COLORS: [Color; 4] = [
        Color::DarkGray,
        Color::Gray,
        Color::LightBlue,
        Color::LightMagenta,
    ];
    match kind {
        ItemKind::Dir => DIR_COLORS[idx % DIR_COLORS.len()],
        ItemKind::File => FILE_COLORS[idx % FILE_COLORS.len()],
        ItemKind::FilesAggregate => Color::LightMagenta,
    }
}

fn text_color(bg: Color) -> Color {
    match bg {
        Color::Yellow
        | Color::LightYellow
        | Color::LightGreen
        | Color::LightBlue
        | Color::Cyan => Color::Black,
        _ => Color::White,
    }
}

fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = bytes as f64;
    let mut unit = 0usize;
    while size >= 1024.0 && unit + 1 < UNITS.len() {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else if size >= 100.0 {
        format!("{:.0} {}", size, UNITS[unit])
    } else if size >= 10.0 {
        format!("{:.1} {}", size, UNITS[unit])
    } else {
        format!("{:.2} {}", size, UNITS[unit])
    }
}

fn fs_usage(path: &Path) -> Option<(u64, u64)> {
    let c = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut vfs: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c.as_ptr(), &mut vfs) };
    if rc != 0 {
        return None;
    }
    let frsize = vfs.f_frsize as u64;
    let total = (vfs.f_blocks as u64).saturating_mul(frsize);
    let avail = (vfs.f_bavail as u64).saturating_mul(frsize);
    let used = total.saturating_sub(avail);
    Some((used, total))
}

fn perform_delete(action: &ConfirmAction) -> Result<(), String> {
    if action.is_dir {
        fs::remove_dir_all(&action.target_path).map_err(|e| format!("Delete failed: {}", e))
    } else {
        fs::remove_file(&action.target_path).map_err(|e| format!("Delete failed: {}", e))
    }
}

fn render_usage_bar(
    f: &mut ratatui::Frame,
    area: Rect,
    used: u64,
    total: u64,
    device_label: &str,
    version_label: &str,
) {
    if area.width < 4 || total == 0 {
        return;
    }
    let pct = ((used as f64 / total as f64) * 100.0).round() as u64;
    let total_w = area.width as usize;
    let version_w = version_label.len();
    let desired_bar = 20usize;
    let min_bar = 10usize;
    let desired_device = device_label.len();

    let mut bar_w = desired_bar.min(total_w.saturating_sub(2));
    let device_w;

    if total_w >= desired_device + bar_w + version_w {
        device_w = desired_device;
    } else if total_w >= desired_device + min_bar + version_w {
        bar_w = total_w - desired_device - version_w;
        device_w = desired_device;
    } else {
        let remaining = total_w.saturating_sub(version_w);
        if remaining >= min_bar {
            bar_w = remaining.min(bar_w);
            device_w = remaining.saturating_sub(bar_w);
        } else {
            bar_w = remaining;
            device_w = 0;
        }
    }

    let mut chunks = Vec::new();
    if device_w > 0 {
        chunks.push(Constraint::Length(device_w as u16));
    }
    chunks.push(Constraint::Length(bar_w as u16));
    if device_w > 0 {
        chunks.push(Constraint::Length(version_w as u16));
    }

    let parts = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(chunks)
        .split(area);

    let mut idx = 0usize;
    if device_w > 0 {
        let device_rect = parts[idx];
        idx += 1;
        let mut label = device_label.to_string();
        if label.len() > device_w {
            label = truncate_middle(&label, device_w);
        }
        let p = Paragraph::new(label).style(Style::default().fg(Color::White));
        f.render_widget(p, device_rect);
    }

    let bar_rect = parts[idx];
    idx += 1;
    let inner_w = bar_rect.width.saturating_sub(2) as usize;
    let filled = ((used as f64 / total as f64) * inner_w as f64).round() as usize;
    let mut bar = String::with_capacity(inner_w);
    for i in 0..inner_w {
        if i < filled {
            bar.push('█');
        } else {
            bar.push('░');
        }
    }
    let label = format!("{:>3}%", pct.min(100));
    let mut chars: Vec<char> = bar.chars().collect();
    let start = inner_w.saturating_sub(label.len());
    for (i, ch) in label.chars().enumerate() {
        if start + i < chars.len() {
            chars[start + i] = ch;
        }
    }
    let final_bar: String = chars.into_iter().collect();

    let p = Paragraph::new(final_bar)
        .style(Style::default().fg(Color::Black).bg(Color::LightGreen))
        .block(Block::default().style(Style::default().bg(Color::DarkGray)));
    f.render_widget(p, bar_rect);

    if device_w > 0 {
        let version_rect = parts[idx];
        let p = Paragraph::new(version_label)
            .style(Style::default().fg(Color::DarkGray))
            .alignment(ratatui::layout::Alignment::Right);
        f.render_widget(p, version_rect);
    }
}

fn current_device(path: &Path) -> Option<String> {
    let canon = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mounts = fs::read_to_string("/proc/self/mounts").ok()?;
    let mut best: Option<(usize, String)> = None;
    for line in mounts.lines() {
        let mut parts = line.split_whitespace();
        let dev = parts.next()?;
        let mnt = parts.next()?;
        let dev = unescape_mount_field(dev);
        let mnt = unescape_mount_field(mnt);
        let mnt_path = Path::new(&mnt);
        if !canon.starts_with(mnt_path) {
            continue;
        }
        let mnt_len = mnt_path.as_os_str().len();
        if let Some((best_len, _)) = &best {
            if mnt_len <= *best_len {
                continue;
            }
        }
        best = Some((mnt_len, dev));
    }
    best.map(|(_, dev)| dev)
}

fn unescape_mount_field(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            let a = chars.next();
            let b = chars.next();
            let c = chars.next();
            match (a, b, c) {
                (Some('0'), Some('4'), Some('0')) => out.push(' '),
                (Some('0'), Some('1'), Some('1')) => out.push('\t'),
                (Some('0'), Some('1'), Some('2')) => out.push('\n'),
                (Some('1'), Some('3'), Some('4')) => out.push('\\'),
                (Some(x), Some(y), Some(z)) => {
                    out.push('\\');
                    out.push(x);
                    out.push(y);
                    out.push(z);
                }
                _ => out.push('\\'),
            }
        } else {
            out.push(ch);
        }
    }
    out
}

fn centered_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let width = (area.width * percent_x) / 100;
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect { x, y, width, height }
}
