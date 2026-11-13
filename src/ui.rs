use crate::app::{App, Content, Editing, GLOBAL_KEYS, PANELS, Panel, Popup, Tab, bindings};
use crate::docker::{Container, short_id};
use crate::logs;
use crate::stats::{self, Sample, format_bytes};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Sparkline, Table, Wrap};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const ACCENT: Color = Color::Green;
const DIM: Style = Style::new().fg(Color::DarkGray);
const KEY: Style = Style::new().fg(Color::Cyan);
const ERROR: Style = Style::new().fg(Color::Red);
const RUNTIME_ROWS_FOCUSED: usize = 9;
const RUNTIME_ROWS: usize = 3;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [body, footer] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());
    let side_width = (body.width / 3).clamp(32.min(body.width), 64);
    let [side, main] = Layout::horizontal([Constraint::Length(side_width), Constraint::Fill(1)]).areas(body);
    draw_side(frame, app, side);
    draw_main(frame, app, main);
    draw_footer(frame, app, footer);
    if let Some(popup) = &app.popup {
        draw_popup(frame, app, popup);
    }
}

fn draw_side(frame: &mut Frame, app: &App, area: Rect) {
    let constraints = PANELS.map(|panel| match panel {
        Panel::Runtimes => Constraint::Length(app.runtimes.len().clamp(1, if app.focus == panel { RUNTIME_ROWS_FOCUSED } else { RUNTIME_ROWS }) as u16 + 2),
        _ if app.focus == panel => Constraint::Fill(3),
        _ => Constraint::Fill(1),
    });
    let areas = Layout::vertical(constraints).split(area);
    for (panel, area) in PANELS.iter().zip(areas.iter()) {
        draw_panel(frame, app, *panel, *area);
    }
}

fn draw_panel(frame: &mut Frame, app: &App, panel: Panel, area: Rect) {
    let focused = app.focus == panel && !app.main_focused;
    let width = area.width.saturating_sub(2) as usize;
    let rows: Vec<Line> = match panel {
        Panel::Runtimes => app
            .runtimes
            .iter()
            .map(|runtime| {
                let connected = app.docker.as_ref().is_some_and(|docker| docker.socket == runtime.socket);
                let running = runtime.running();
                let state = if connected {
                    "connected"
                } else if running {
                    "running"
                } else {
                    "stopped"
                };
                let color = if connected {
                    ACCENT
                } else if running {
                    Color::Reset
                } else {
                    Color::DarkGray
                };
                row(dot(if running { "●" } else { "○" }, color), &runtime.name, state, width, Style::new())
            })
            .collect(),
        Panel::Projects => app
            .visible_projects()
            .iter()
            .map(|project| {
                let (running, total) = (project.running(), project.containers.len());
                let color = if total > 0 && running == total {
                    ACCENT
                } else if running > 0 {
                    Color::Yellow
                } else {
                    Color::DarkGray
                };
                let samples: Vec<_> = project.containers.iter().filter(|container| container.running()).filter_map(|container| app.stats.get(&container.id)?.back()).collect();
                let right = if total == 0 {
                    "not created".into()
                } else if samples.is_empty() {
                    format!("{running}/{total}")
                } else {
                    usage(&samples)
                };
                row(dot(if running > 0 { "●" } else { "○" }, color), &project.name, &right, width, Style::new())
            })
            .collect(),
        Panel::Containers => app
            .containers()
            .iter()
            .map(|container| {
                let right = match app.stats.get(&container.id).and_then(|history| history.back()) {
                    Some(sample) if container.running() => usage(&[sample]),
                    _ => String::new(),
                };
                row(dot(state_glyph(container), state_color(container)), &container.name, &right, width, Style::new())
            })
            .collect(),
        Panel::Images => app
            .images()
            .iter()
            .map(|image| {
                let style = if app.image_used(image) { Style::new() } else { DIM };
                let name = if image.reference == image.id { short_id(&image.id) } else { &image.reference };
                row(Span::raw(""), name, &format_bytes(image.size), width, style)
            })
            .collect(),
        Panel::Volumes => app
            .volumes()
            .iter()
            .map(|volume| {
                let style = if app.volume_used(volume) { Style::new() } else { DIM };
                row(Span::raw(""), &app.volume_label(volume), &volume.driver, width, style)
            })
            .collect(),
        Panel::Networks => app
            .networks()
            .iter()
            .map(|network| {
                let style = if app.network_used(network) { Style::new() } else { DIM };
                row(Span::raw(""), &network.name, format!("{} {}", network.driver, network.subnet).trim_end(), width, style)
            })
            .collect(),
    };

    let count = match panel {
        Panel::Containers => format!("{}, {} running ", rows.len(), app.snapshot.containers.iter().filter(|container| container.running()).count()),
        _ => format!("{} ", rows.len()),
    };
    let mut block = Block::bordered()
        .title(Line::from(vec![Span::styled(format!(" {} ", panel.index() + 1), KEY), Span::raw(format!("{} ", panel.title())), Span::styled(count, DIM)]))
        .border_style(if focused { Style::new().fg(ACCENT) } else { DIM });
    let filter = &app.filters[panel.index()];
    let editing = app.editing == Some(Editing::List) && app.focus == panel;
    if editing || !filter.is_empty() {
        block = block.title_bottom(Line::from(format!(" /{filter}{} ", if editing { "_" } else { "" })).style(Style::new().fg(Color::Yellow)));
    }
    if app.narrow[panel.index()] {
        block = block.title_bottom(Line::from(format!(" only {} ", panel.narrow_label())).right_aligned().style(Style::new().fg(Color::Yellow)));
    }
    let selected = if app.focus == panel || panel == Panel::Runtimes { app.selected_index(panel) } else { None };
    let (rows, selected) = with_more_row(rows, selected, area.height.saturating_sub(2) as usize);
    let list = List::new(rows.into_iter().map(ListItem::new)).block(block).highlight_style(highlight(focused));
    let mut state = ListState::default().with_selected(selected);
    frame.render_stateful_widget(list, area, &mut state);
}

/// Rows that overflow `height` give up the last line to an `N more` row; the window follows the selection.
fn with_more_row(mut rows: Vec<Line>, selected: Option<usize>, height: usize) -> (Vec<Line>, Option<usize>) {
    if rows.len() <= height || height < 2 {
        return (rows, selected);
    }
    let shown = height - 1;
    let offset = selected.map_or(0, |index| index.saturating_sub(shown - 1)).min(rows.len() - shown);
    let (above, below) = (offset, rows.len() - offset - shown);
    let mut window: Vec<Line> = rows.drain(offset..offset + shown).collect();
    let more = if below > 0 { format!("{below} more") } else { format!("{above} more above") };
    window.push(Line::from(more).style(DIM));
    (window, selected.map(|index| index - offset))
}

/// `glyph name ... right`, the name cut to leave room for the right column.
fn row<'a>(glyph: Span<'a>, name: &str, right: &str, width: usize, style: Style) -> Line<'a> {
    let glyph_width = if glyph.content.is_empty() { 0 } else { glyph.width() + 1 };
    let right = &truncate(right, width / 2);
    let right_width = right.width() + 1;
    let name = truncate(name, width.saturating_sub(glyph_width + right_width));
    let gap = width.saturating_sub(glyph_width + name.width() + right.width());
    let mut spans = vec![];
    if glyph_width > 0 {
        spans.extend([glyph, Span::raw(" ")]);
    }
    spans.extend([Span::styled(name, style), Span::raw(" ".repeat(gap)), Span::styled(right.to_string(), DIM)]);
    Line::from(spans)
}

fn dot(glyph: &'static str, color: Color) -> Span<'static> {
    Span::styled(glyph, Style::new().fg(color))
}

fn highlight(focused: bool) -> Style {
    // Unfocused keeps a dimmer bar (256-color 236) so the selection stays visible while the main panel has focus.
    let bar = if focused { Color::DarkGray } else { Color::Indexed(236) };
    Style::new().bg(bar).add_modifier(Modifier::BOLD)
}

/// `48.2M   2.3%`, memory and CPU summed over `samples`, in fixed-width columns.
fn usage(samples: &[&Sample]) -> String {
    let memory = format_bytes(samples.iter().map(|sample| sample.memory_used).sum()).replace("iB", "").replace(' ', "");
    let cpu: Option<f64> = samples.iter().filter_map(|sample| sample.cpu_percent).reduce(|a, b| a + b);
    format!("{memory:>6} {:>6}", cpu.map_or("-".into(), |cpu| format!("{cpu:.1}%")))
}

fn state_glyph(container: &Container) -> &'static str {
    match container.state.as_str() {
        "running" => "●",
        "paused" => "‖",
        "restarting" => "↻",
        "created" => "◌",
        "dead" | "removing" => "✕",
        _ => "○",
    }
}

fn state_color(container: &Container) -> Color {
    match container.state.as_str() {
        _ if container.status.contains("(unhealthy)") => Color::Red,
        "running" => ACCENT,
        "paused" | "restarting" => Color::Yellow,
        "dead" => Color::Red,
        _ => Color::DarkGray,
    }
}

fn truncate(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let char_width = c.width().unwrap_or(0);
        if used + char_width + 1 > width {
            break;
        }
        out.push(c);
        used += char_width;
    }
    if width > 0 {
        out.push('~');
    }
    out
}

fn draw_main(frame: &mut Frame, app: &mut App, area: Rect) {
    let tabs = app.focus.tabs();
    let mut title = vec![Span::raw(" ")];
    for (index, tab) in tabs.iter().enumerate() {
        // Explicit colors on both, since the title otherwise takes the border color when focused.
        let style = if index == app.tab_index() { Style::new().fg(Color::Black).bg(ACCENT).add_modifier(Modifier::BOLD) } else { Style::new().fg(Color::Gray) };
        title.push(Span::styled(format!(" {tab:?} "), style));
    }
    title.push(Span::raw(" "));
    let mut block = Block::bordered().title(Line::from(title)).border_style(if app.main_focused { Style::new().fg(ACCENT) } else { DIM });
    if let Some(bottom) = main_status(app) {
        block = block.title_bottom(Line::from(bottom).right_aligned().style(Style::new().fg(Color::Yellow)));
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);
    app.page = inner.height as usize;

    if app.docker.is_none() && app.focus != Panel::Runtimes && !(app.focus == Panel::Projects && app.tab() == Tab::Compose) {
        let chosen = app.runtimes.iter().find(|runtime| Some(&runtime.socket) == app.chosen.as_ref());
        let text = match chosen {
            Some(runtime) => match &runtime.error {
                Some(error) => format!("{} is not reachable: {error}\nStart it, or pick another runtime in panel 1.", runtime.name),
                None => format!("Connecting to {}...", runtime.name),
            },
            None if app.runtimes.is_empty() => "Looking for Docker runtimes...".into(),
            None => "No Docker runtime is running.".into(),
        };
        return placeholder(frame, inner, &text);
    }
    match app.tab() {
        Tab::Engine => draw_engine(frame, app, inner),
        Tab::Logs => draw_logs(frame, app, inner),
        Tab::Stats => draw_stats(frame, app, inner),
        Tab::Services => draw_services(frame, app, inner),
        Tab::Files => draw_files(frame, app, inner),
        _ => draw_content(frame, app, inner),
    }
}

fn main_status(app: &App) -> Option<String> {
    let mut parts = Vec::new();
    if app.editing == Some(Editing::Main) || !app.main_filter.is_empty() {
        parts.push(format!("/{}{}", app.main_filter, if app.editing == Some(Editing::Main) { "_" } else { "" }));
    }
    if app.tab() == Tab::Logs && app.view.scroll > 0 {
        parts.push(format!("{} lines up, G follows", app.view.scroll));
    }
    (!parts.is_empty()).then(|| format!(" {} ", parts.join("  ")))
}

fn placeholder(frame: &mut Frame, area: Rect, text: &str) {
    frame.render_widget(Paragraph::new(text.to_string()).style(DIM).wrap(Wrap { trim: false }), area);
}

fn failed(text: &str) -> Paragraph<'_> {
    Paragraph::new(text).style(ERROR).wrap(Wrap { trim: false })
}

fn draw_engine(frame: &mut Frame, app: &App, area: Rect) {
    let Some(runtime) = app.selected_runtime() else { return placeholder(frame, area, "No Docker runtime found. Start one, then press R.") };
    let connected = app.docker.as_ref().is_some_and(|docker| docker.socket == runtime.socket);
    let field = |name: &str, value: String| Line::from(vec![Span::styled(format!("{name:<10}"), DIM), Span::raw(value)]);
    let mut lines = vec![field("Runtime", runtime.name.clone()), field("Socket", runtime.socket.clone())];
    match (&app.engine, connected) {
        (Some(Ok(engine)), true) => {
            let snapshot = &app.snapshot;
            let running = snapshot.containers.iter().filter(|container| container.running()).count();
            let latest: Vec<_> = app.stats.values().filter_map(|history| history.back()).collect();
            let cpu: f64 = latest.iter().filter_map(|sample| sample.cpu_percent).sum();
            let memory: u64 = latest.iter().map(|sample| sample.memory_used).sum();
            lines.extend([
                field("Engine", format!("Docker {} (API {})", engine.version, engine.api_version)),
                field("System", format!("{}, kernel {}", engine.system, engine.kernel)),
                field("Machine", format!("{}, {} CPUs, {}", engine.architecture, engine.cpus, format_bytes(engine.memory))),
                // Docker's CPU percent counts each core as 100.
                field("Usage", format!("{:.1} % of {} CPUs, {} of {}", cpu / engine.cpus.max(1) as f64, engine.cpus, format_bytes(memory), format_bytes(engine.memory))),
                field("Host", engine.name.clone()),
                field(
                    "Objects",
                    format!("{} containers ({running} running), {} images, {} volumes, {} networks", snapshot.containers.len(), snapshot.images.len(), snapshot.volumes.len(), snapshot.networks.len()),
                ),
            ]);
        }
        (Some(Err(error)), true) => lines.push(Line::styled(format!("{:<10}{error}", "Engine"), ERROR)),
        (None, true) => lines.push(field("Engine", "loading...".into())),
        _ => match &runtime.error {
            None => lines.push(field("State", "running, press Enter to switch to it".into())),
            Some(error) => lines.push(field("State", error.clone())),
        },
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn draw_logs(frame: &mut Frame, app: &App, area: Rect) {
    let query = app.main_filter.to_lowercase();
    let shown: Vec<&logs::LogLine> = app.view.logs.iter().filter(|line| logs::matches(line, &query)).collect();
    if shown.is_empty() {
        let text = match &app.view.content {
            Content::Failed(error) => error.as_str(),
            _ if !query.is_empty() => "No line matches the filter.",
            _ => "No logs yet.",
        };
        return placeholder(frame, area, text);
    }
    let (width, height) = (area.width as usize, area.height as usize);
    let end = shown.len().saturating_sub(app.view.scroll);
    let mut rows: Vec<Line> = Vec::new();
    for line in shown[..end].iter().rev() {
        let mut prefix = Vec::new();
        // Docker stamps lines in UTC; Apple's container CLI does not stamp them.
        if app.timestamps && !line.timestamp.is_empty() {
            prefix.push(Span::styled(format!("{}Z ", line.time()), DIM));
        }
        if let Some(source) = &line.source {
            prefix.push(Span::styled(format!("{source} | "), Style::new().fg(source_color(source))));
        }
        let prefix_width: usize = prefix.iter().map(Span::width).sum();
        let pieces = wrap(&line.text, width.saturating_sub(prefix_width).max(1), width.max(1));
        let mut wrapped: Vec<Line> = Vec::new();
        for (index, piece) in pieces.into_iter().enumerate() {
            if index == 0 {
                let mut spans = prefix.clone();
                spans.push(Span::raw(piece));
                wrapped.push(Line::from(spans));
            } else {
                wrapped.push(Line::from(piece));
            }
        }
        rows.extend(wrapped.into_iter().rev());
        if rows.len() >= height {
            break;
        }
    }
    rows.truncate(height);
    rows.reverse();
    frame.render_widget(Paragraph::new(rows), area);
}

/// Cuts `text` into rows by display width: the first `first` columns wide, the rest `rest` wide.
fn wrap(text: &str, first: usize, rest: usize) -> Vec<String> {
    let mut rows = vec![String::new()];
    let mut used = 0;
    for c in text.chars() {
        let char_width = c.width().unwrap_or(0);
        let limit = if rows.len() == 1 { first } else { rest };
        if used + char_width > limit && used > 0 {
            rows.push(String::new());
            used = 0;
        }
        rows.last_mut().unwrap().push(c);
        used += char_width;
    }
    rows
}

fn source_color(source: &str) -> Color {
    const COLORS: [Color; 6] = [Color::Cyan, Color::Magenta, Color::Yellow, Color::Blue, Color::LightGreen, Color::LightRed];
    COLORS[source.bytes().map(usize::from).sum::<usize>() % COLORS.len()]
}

fn draw_stats(frame: &mut Frame, app: &App, area: Rect) {
    let Some(container) = app.selected_container() else { return };
    if !container.running() {
        return placeholder(frame, area, "Start the container to see its stats.");
    }
    let Some(latest) = app.stats.get(&container.id).and_then(|history| history.back()) else {
        return placeholder(frame, area, "Waiting for the first reading...");
    };
    let history = &app.stats[&container.id];
    let field = |name: &str, value: String| Line::from(vec![Span::styled(format!("{name:<9}"), DIM), Span::raw(value)]);
    let memory_share = if latest.memory_limit > 0 { format!(" ({:.1} %)", latest.memory_used as f64 / latest.memory_limit as f64 * 100.0) } else { String::new() };
    let readouts = vec![
        field("CPU", latest.cpu_percent.map_or("-".into(), |cpu| format!("{cpu:.1} %"))),
        field("Memory", format!("{} of {}{memory_share}", format_bytes(latest.memory_used), format_bytes(latest.memory_limit))),
        field("Network", format!("{} in, {} out", format_bytes(latest.net_rx), format_bytes(latest.net_tx))),
        field("Disk", format!("{} read, {} written", format_bytes(latest.block_read), format_bytes(latest.block_write))),
    ];
    let [top, cpu_area, memory_area] = Layout::vertical([Constraint::Length(5), Constraint::Fill(1), Constraint::Fill(1)]).areas(area);
    frame.render_widget(Paragraph::new(readouts), top);

    let cpu: Vec<u64> = history.iter().map(|sample| (sample.cpu_percent.unwrap_or(0.0) * 10.0) as u64).collect();
    let cpu_peak = cpu.iter().copied().max().unwrap_or(0);
    let cpu_top = cpu_peak.max(10);
    let cpu_title = format!(" CPU, last {} readings, peak {:.1} % ", history.len(), cpu_peak as f64 / 10.0);
    frame.render_widget(Sparkline::default().block(Block::bordered().title(cpu_title).border_style(DIM)).data(cpu).max(cpu_top).style(Style::new().fg(Color::Cyan)), cpu_area);

    let memory: Vec<u64> = history.iter().map(|sample| sample.memory_used).collect();
    let memory_peak = memory.iter().copied().max().unwrap_or(0);
    let memory_title = format!(" Memory, peak {} ", format_bytes(memory_peak));
    let memory_top = memory_peak.max(1024 * 1024);
    frame.render_widget(Sparkline::default().block(Block::bordered().title(memory_title).border_style(DIM)).data(memory).max(memory_top).style(Style::new().fg(ACCENT)), memory_area);
}

fn draw_services(frame: &mut Frame, app: &App, area: Rect) {
    let Some(project) = app.selected_project() else { return };
    if project.containers.is_empty() {
        return placeholder(frame, area, &format!("{} has no containers yet. Press u to run docker compose up.", project.name));
    }
    let header = Row::new(["SERVICE", "STATE", "STATUS", "CPU", "MEMORY", "PORTS"]).style(DIM);
    let rows = project.containers.iter().map(|container| {
        let sample = app.stats.get(&container.id).and_then(|history| history.back()).filter(|_| container.running());
        Row::new([
            Cell::from(container.service.clone().unwrap_or(container.name.clone())).style(Style::new().fg(state_color(container))),
            Cell::from(container.state.clone()),
            Cell::from(container.status.clone()),
            Cell::from(sample.and_then(|sample| sample.cpu_percent).map_or(String::new(), |cpu| format!("{cpu:.1}%"))),
            Cell::from(sample.map_or(String::new(), |sample| format_bytes(sample.memory_used))),
            Cell::from(container.ports.clone()),
        ])
    });
    let widths = [Constraint::Fill(2), Constraint::Length(10), Constraint::Fill(3), Constraint::Length(7), Constraint::Length(10), Constraint::Fill(2)];
    frame.render_widget(Table::new(rows, widths).header(header), area);
}

fn draw_content(frame: &mut Frame, app: &mut App, area: Rect) {
    let lines = match &app.view.content {
        Content::Loading => return placeholder(frame, area, "Loading..."),
        Content::Failed(error) => return frame.render_widget(failed(error), area),
        Content::Lines(lines) if lines.is_empty() => return placeholder(frame, area, "Empty."),
        Content::Lines(lines) => lines,
    };
    let query = app.main_filter.to_lowercase();
    let width = (area.width as usize).max(1);
    let mut rows = Vec::new();
    for line in lines.iter().filter(|line| query.is_empty() || line.to_lowercase().contains(&query)) {
        for piece in wrap(line, width, width) {
            rows.push(piece);
        }
    }
    if rows.is_empty() {
        return placeholder(frame, area, "No line matches the filter.");
    }
    app.view.scroll = app.view.scroll.min(rows.len().saturating_sub(area.height as usize));
    let inspect = app.tab() == Tab::Inspect;
    let visible: Vec<Line> = rows.into_iter().skip(app.view.scroll).take(area.height as usize).map(|row| if inspect { json_line(row) } else { Line::from(row) }).collect();
    frame.render_widget(Paragraph::new(visible), area);
}

/// Colors the key of a pretty-printed JSON line; a wrapped piece without `": ` stays plain.
fn json_line(row: String) -> Line<'static> {
    match row.split_once("\": ") {
        Some((key, value)) if key.trim_start().starts_with('"') => Line::from(vec![Span::styled(format!("{key}\":"), KEY), Span::raw(format!(" {value}"))]),
        _ => Line::from(row),
    }
}

fn draw_files(frame: &mut Frame, app: &App, area: Rect) {
    let files = &app.view.files;
    let [list_area, preview_area] = Layout::horizontal([Constraint::Percentage(45), Constraint::Fill(1)]).areas(area);
    let list_block = Block::bordered().title(format!(" {} ", files.folder)).border_style(DIM);
    let entries = match &files.entries {
        None => return frame.render_widget(Paragraph::new("Loading...").style(DIM).block(list_block), list_area),
        Some(Err(error)) => return frame.render_widget(failed(&error.message).block(list_block), list_area),
        Some(Ok(entries)) => entries,
    };
    let width = list_area.width.saturating_sub(2) as usize;
    let rows: Vec<ListItem> = entries
        .iter()
        .map(|entry| {
            let (name, right, style) = match entry.kind.as_str() {
                "directory" => (format!("{}/", entry.name), String::new(), Style::new().fg(Color::Blue)),
                "regular file" | "regular empty file" => (entry.name.clone(), format_bytes(entry.size), Style::new()),
                kind => (entry.name.clone(), kind.to_string(), DIM),
            };
            ListItem::new(row(Span::raw(""), &name, &right, width, style))
        })
        .collect();
    let list = List::new(rows).block(list_block.title_bottom(Line::from(" Enter opens, h goes up, S saves, U uploads ").style(DIM))).highlight_style(highlight(app.main_focused));
    let mut state = ListState::default().with_selected((!entries.is_empty()).then_some(files.selected));
    frame.render_stateful_widget(list, list_area, &mut state);

    let selected = entries.get(files.selected);
    let title = selected.map_or(String::new(), |entry| format!(" {} {} {} ", entry.mode, entry.name, stats::age(entry.modified)));
    let preview_block = Block::bordered().title(title).title_bottom(Line::from(" PgUp PgDn scroll ").style(DIM)).border_style(DIM);
    let preview = match &files.preview {
        Content::Loading => Paragraph::new("Loading...").style(DIM),
        Content::Failed(error) => failed(error),
        Content::Lines(lines) => Paragraph::new(lines.iter().skip(files.preview_scroll).take(preview_area.height as usize).map(|line| Line::from(line.as_str())).collect::<Vec<_>>()),
    };
    frame.render_widget(preview.block(preview_block), preview_area);
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let status = if let Some(notice) = &app.notice {
        Span::styled(format!(" {} ", notice.text), Style::new().fg(if notice.error { Color::Red } else { ACCENT }))
    } else if !app.busy.is_empty() {
        Span::styled(format!(" {}... ", app.busy.join(", ")), Style::new().fg(Color::Yellow))
    } else {
        Span::raw("")
    };
    let [hints_area, status_area] = Layout::horizontal([Constraint::Fill(1), Constraint::Length(status.width().min(area.width as usize / 2) as u16)]).areas(area);
    let mut hints = Vec::new();
    let navigation: &[(&str, &str)] = match (app.main_focused, app.focus) {
        (true, _) => &[("Esc", "back"), ("[ ]", "tab")],
        (false, Panel::Runtimes) => &[("Tab", "panel"), ("Enter", "connect"), ("[ ]", "tab")],
        (false, _) => &[("Tab", "panel"), ("Enter", "open"), ("[ ]", "tab")],
    };
    for (key, label) in navigation {
        hints.extend([Span::styled(*key, KEY), Span::raw(format!(" {label}  "))]);
    }
    for binding in bindings(app.focus).iter().chain(GLOBAL_KEYS.iter().filter(|binding| binding.key != '?')) {
        hints.extend([Span::styled(binding.key.to_string(), KEY), Span::raw(format!(" {}  ", binding.label))]);
    }
    frame.render_widget(Paragraph::new(Line::from(hints)), hints_area);
    frame.render_widget(Paragraph::new(Line::from(status)).right_aligned(), status_area);
}

fn draw_popup(frame: &mut Frame, app: &App, popup: &Popup) {
    let (title, lines, color): (String, Vec<Line>, Color) = match popup {
        Popup::Confirm { title, body, .. } => (title.clone(), vec![Line::from(body.as_str()), Line::raw(""), hint_line(&[("y", "confirm"), ("n", "cancel")])], Color::Yellow),
        Popup::Pull { value } => (
            "Pull image".into(),
            vec![Line::from(format!("> {value}_")), Line::raw(""), Line::styled("Without a tag or digest, the latest tag is pulled.", DIM), hint_line(&[("Enter", "pull"), ("Esc", "cancel")])],
            ACCENT,
        ),
        Popup::Upload { value } => (
            format!("Upload into {}", app.view.files.folder),
            vec![Line::from(format!("> {value}_")), Line::raw(""), Line::styled("A host file or folder; ~/ is your home.", DIM), hint_line(&[("Enter", "upload"), ("Esc", "cancel")])],
            ACCENT,
        ),
        Popup::Menu { selected } => {
            let menu = app.menu();
            let lines = menu
                .iter()
                .enumerate()
                .map(|(index, binding)| {
                    let style = if index == *selected { highlight(true) } else { Style::new() };
                    Line::from(vec![Span::styled(format!(" {:<6}", binding.key), KEY), Span::raw(binding.label)]).style(style)
                })
                .collect();
            (format!("{} commands", app.focus.title()), lines, ACCENT)
        }
        Popup::Error { title, body } => (title.clone(), body.lines().map(|line| Line::from(line.to_string())).chain([Line::raw(""), hint_line(&[("any key", "close")])]).collect(), Color::Red),
    };
    let area = frame.area();
    let width = 72.min(area.width.saturating_sub(4)).max(20);
    let inner_width = width.saturating_sub(2).max(1);
    let body = Paragraph::new(lines).wrap(Wrap { trim: false });
    let height = body.line_count(inner_width) as u16 + 2;
    let popup_area = Rect { x: area.x + (area.width.saturating_sub(width)) / 2, y: area.y + area.height.saturating_sub(height) / 3, width, height: height.min(area.height) };
    frame.render_widget(Clear, popup_area);
    let block = Block::bordered().title(format!(" {title} ")).border_style(Style::new().fg(color));
    frame.render_widget(body.block(block), popup_area);
}

fn hint_line(hints: &[(&str, &str)]) -> Line<'static> {
    let mut spans = Vec::new();
    for (key, label) in hints {
        spans.extend([Span::styled(key.to_string(), KEY), Span::raw(format!(" {label}  "))]);
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_the_active_tab_while_the_main_panel_has_focus() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(tx, None);
        app.main_focused = true;
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let title: String = (0..120).map(|x| buffer[(x, 0)].symbol()).collect();
        let column = |name: &str| title[..title.find(name).unwrap()].chars().count() as u16;
        assert_eq!(buffer[(column("Logs"), 0)].bg, ACCENT);
        assert_ne!(buffer[(column("Stats"), 0)].bg, ACCENT);
        assert_ne!(buffer[(column("Stats"), 0)].fg, ACCENT);
    }

    #[test]
    fn wraps_by_display_width() {
        assert_eq!(wrap("abcdefgh", 3, 4), ["abc", "defg", "h"]);
        assert_eq!(wrap("", 3, 4), [""]);
        assert_eq!(wrap("日本語", 4, 4), ["日本", "語"]);
    }

    #[test]
    fn truncates_with_a_marker() {
        assert_eq!(truncate("container-name", 8), "contain~");
        assert_eq!(truncate("short", 8), "short");
    }

    #[test]
    fn overflow_gives_the_last_line_to_a_more_row() {
        let lines = |count: usize| (0..count).map(|index| Line::from(index.to_string())).collect::<Vec<_>>();
        let text = |rows: &[Line]| rows.iter().map(|line| line.to_string()).collect::<Vec<_>>();
        let (rows, selected) = with_more_row(lines(3), Some(2), 3);
        assert_eq!((text(&rows), selected), (vec!["0".into(), "1".into(), "2".into()], Some(2)));
        let (rows, selected) = with_more_row(lines(10), None, 4);
        assert_eq!((text(&rows), selected), (vec!["0".into(), "1".into(), "2".into(), "7 more".into()], None));
        let (rows, selected) = with_more_row(lines(10), Some(5), 4);
        assert_eq!((text(&rows), selected), (vec!["3".into(), "4".into(), "5".into(), "4 more".into()], Some(2)));
        let (rows, selected) = with_more_row(lines(10), Some(9), 4);
        assert_eq!((text(&rows), selected), (vec!["7".into(), "8".into(), "9".into(), "7 more above".into()], Some(2)));
    }
}
