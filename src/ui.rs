use crate::app::{App, Click, DragItem, Drop, InputKind, Mode, TileRow};
use crate::date;
use crate::model::Task;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};
use ratatui::Frame;

// Catppuccin Mocha, explicit so the panel stays opaque over the translucent backdrop.
const CRUST: Color = Color::Rgb(0x11, 0x11, 0x1b);
const MANTLE: Color = Color::Rgb(0x18, 0x18, 0x25);
const BASE: Color = Color::Rgb(0x1e, 0x1e, 0x2e);
const SURFACE0: Color = Color::Rgb(0x31, 0x32, 0x44);
const SURFACE1: Color = Color::Rgb(0x45, 0x47, 0x5a);
const OVERLAY: Color = Color::Rgb(0x7f, 0x84, 0x9c);
const TEXT: Color = Color::Rgb(0xcd, 0xd6, 0xf4);
const MAUVE: Color = Color::Rgb(0xcb, 0xa6, 0xf7);
const RED: Color = Color::Rgb(0xf3, 0x8b, 0xa8);
const YELLOW: Color = Color::Rgb(0xf9, 0xe2, 0xaf);
const GREEN: Color = Color::Rgb(0xa6, 0xe3, 0xa1);
const SKY: Color = Color::Rgb(0x89, 0xdc, 0xeb);

const ACCENT: Color = MAUVE;
const DIM: Style = Style::new().fg(OVERLAY);
const BTN: Style = Style::new().fg(MAUVE).bg(SURFACE0);
const BTN_HOVER: Style = Style::new().fg(CRUST).bg(MAUVE);
const SELECTED_BG: Color = SURFACE0;
/// Narrowest column before the board starts scrolling sideways.
const MIN_COL_W: u16 = 34;

/// A run of spans with clickable parts, positioned relative to its own start.
#[derive(Default)]
struct Seg {
    spans: Vec<Span<'static>>,
    w: u16,
    /// (x, width, action, span index, style when hovered)
    hits: Vec<(u16, u16, Click, usize, Option<Style>)>,
}

impl Seg {
    fn text(&mut self, s: impl Into<String>, style: Style) -> &mut Self {
        let s = s.into();
        self.w += s.chars().count() as u16;
        self.spans.push(Span::styled(s, style));
        self
    }

    fn click(&mut self, s: impl Into<String>, style: Style, hover: Option<Style>, c: Click) -> &mut Self {
        let s = s.into();
        let w = s.chars().count() as u16;
        self.hits.push((self.w, w, c, self.spans.len(), hover));
        self.text(s, style)
    }

    fn button(&mut self, label: &str, c: Click) -> &mut Self {
        self.click(format!(" {label} "), BTN, Some(BTN_HOVER), c)
    }

    fn buttons(&mut self, items: Vec<(&str, Click)>) -> &mut Self {
        for (i, (label, c)) in items.into_iter().enumerate() {
            if i > 0 {
                self.text(" ", Style::new());
            }
            self.button(label, c);
        }
        self
    }

    fn append(&mut self, other: Seg) -> &mut Self {
        let base = self.spans.len();
        for (x, w, c, si, h) in other.hits {
            self.hits.push((x + self.w, w, c, si + base, h));
        }
        self.w += other.w;
        self.spans.extend(other.spans);
        self
    }
}

fn hovered(app: &App, r: Rect) -> bool {
    app.drag.is_none() && app.hover.is_some_and(|p| r.contains(p))
}

/// Renders a segment at (x, y) clipped to `max_w` and registers its click regions.
fn place(f: &mut Frame, app: &mut App, x: u16, y: u16, max_w: u16, mut seg: Seg) {
    let w = seg.w.min(max_w);
    for (sx, sw, c, si, hover) in seg.hits {
        if sx >= w {
            continue;
        }
        let r = Rect::new(x + sx, y, sw.min(w - sx), 1);
        if let Some(hs) = hover.filter(|_| hovered(app, r)) {
            seg.spans[si].style = seg.spans[si].style.patch(hs);
        }
        app.hits.push((r, c));
    }
    f.render_widget(Line::from(seg.spans), Rect::new(x, y, w, 1));
}

fn block(title: impl Into<Line<'static>>, focused: bool) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(if focused { ACCENT } else { SURFACE1 }))
        .title(title)
}

/// Opaque surface for popups; `Clear` alone would punch a see-through hole.
fn surface(f: &mut Frame, r: Rect, bg: Color) {
    f.render_widget(Clear, r);
    f.render_widget(Block::new().style(Style::new().bg(bg).fg(TEXT)), r);
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else if max == 0 {
        String::new()
    } else {
        s.chars().take(max - 1).chain(['…']).collect()
    }
}

/// In pocket mode the window covers the screen and the panel floats in the middle,
/// leaving a dimmed, blurred margin (translucent terminal background) around it.
fn panel_rect(area: Rect, pocket: bool) -> Rect {
    if !pocket {
        return area;
    }
    let mx = (area.width / 14).max(2);
    let top = (area.height / 12).max(2);
    let bottom = (area.height / 14).max(1);
    Rect::new(
        area.x + mx,
        area.y + top,
        area.width.saturating_sub(2 * mx),
        area.height.saturating_sub(top + bottom),
    )
}

pub fn draw(f: &mut Frame, app: &mut App) {
    app.task_drop = app.drag.as_ref().filter(|d| d.moved).and_then(|d| app.task_drop_target(d.pos));
    app.hits.clear();
    app.tile_rects.clear();
    app.col_rects.clear();
    let panel = panel_rect(f.area(), app.pocket);
    app.panel = panel;
    let frame = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(SURFACE1))
        .style(Style::new().bg(MANTLE).fg(TEXT));
    let inner = frame.inner(panel);
    f.render_widget(Clear, panel);
    f.render_widget(frame, panel);

    let [bar, _, main, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(inner);
    if main.height >= 20 {
        let [board, details] = Layout::vertical([Constraint::Min(5), Constraint::Length(7)]).areas(main);
        draw_board(f, app, board);
        draw_details(f, app, details);
    } else {
        draw_board(f, app, main);
    }
    draw_toolbar(f, app, bar);
    draw_status(f, app, status);
    draw_drag(f, app);
    app.follow_sel = false;

    app.modal_from = app.hits.len();
    match &app.mode {
        Mode::Input { .. } => draw_input(f, app),
        Mode::Confirm { msg, .. } => {
            let msg = msg.clone();
            draw_confirm(f, app, &msg)
        }
        Mode::Help => draw_help(f),
        Mode::MoveTo { list, task, sel } => {
            let (list, task, sel) = (list.clone(), task.clone(), *sel);
            draw_move(f, app, &list, &task, sel)
        }
        Mode::Normal => {}
    }
}

fn draw_toolbar(f: &mut Frame, app: &mut App, area: Rect) {
    let mut title = Seg::default();
    title.text(" ✔ Pocket Tasks", Style::new().fg(ACCENT).bold());
    let all_collapsed = !app.st.lists.is_empty() && app.st.lists.iter().all(|l| app.is_collapsed(&l.id));
    let mut btns = Seg::default();
    btns.buttons(vec![
        ("+ Список", Click::AddList),
        (if app.st.show_completed { "☑ Виконані" } else { "☐ Виконані" }, Click::ToggleCompleted),
        (if all_collapsed { "⊞ Розгорнути" } else { "⊟ Згорнути" }, Click::ExpandAll(all_collapsed)),
        ("⟳", Click::Refresh),
        ("?", Click::Help),
        (if app.pocket { "—" } else { "✕" }, Click::Hide),
    ]);
    let bx = area.x + area.width.saturating_sub(btns.w + 1);
    if title.w + btns.w + 2 <= area.width {
        place(f, app, area.x, area.y, area.width, title);
    }
    place(f, app, bx, area.y, area.width, btns);
}

fn due_label(t: &Task) -> (String, Style) {
    let today = date::today();
    match t.due_date() {
        Some(d) if !t.done() => {
            let st = if d < today {
                Style::new().fg(RED).bold()
            } else if d == today {
                Style::new().fg(YELLOW).bold()
            } else if (d - today).num_days() <= 1 {
                Style::new().fg(SKY)
            } else {
                DIM
            };
            (date::human(d, today), st)
        }
        Some(d) => (date::human(d, today), DIM),
        None => (String::new(), Style::new()),
    }
}

/// Height a tile wants: borders plus its rows (the header sits on the top border).
fn tile_height(app: &App, list: &str) -> u16 {
    2 + app.tile_rows(list).len().saturating_sub(1) as u16
}

fn draw_board(f: &mut Frame, app: &mut App, area: Rect) {
    let layout = app.st.layout.clone();
    if layout.is_empty() {
        let mut s = Seg::default();
        s.text(" Списків ще немає  ", DIM).button("+ Створити список", Click::AddList);
        place(f, app, area.x, area.y, area.width, s);
        return;
    }
    let ncols = layout.len();
    let vis = ncols.min((area.width / MIN_COL_W).max(1) as usize);
    if app.follow_sel {
        if let Some((c, _)) = app.tile_pos(&app.sel.list) {
            if c < app.col_offset {
                app.col_offset = c;
            } else if c >= app.col_offset + vis {
                app.col_offset = c + 1 - vis;
            }
        }
    }
    app.col_offset = app.col_offset.min(ncols - vis);
    app.col_scroll.resize(ncols, 0);

    let col_w = area.width / vis as u16;
    for k in 0..vis {
        let c = app.col_offset + k;
        let x = area.x + k as u16 * col_w;
        let w = if k + 1 == vis { area.right() - x } else { col_w };
        draw_column(f, app, c, &layout[c], Rect::new(x, area.y, w, area.height));
    }

    // Arrows when some columns are off-screen.
    if app.col_offset > 0 {
        let mut s = Seg::default();
        s.button(&format!("‹ {}", app.col_offset), Click::ColScroll(usize::MAX, -1));
        place(f, app, area.x, area.bottom() - 1, s.w, s);
    }
    let right = ncols - app.col_offset - vis;
    if right > 0 {
        let mut s = Seg::default();
        s.button(&format!("{right} ›"), Click::ColScroll(usize::MAX, 1));
        place(f, app, area.right().saturating_sub(s.w), area.bottom() - 1, s.w, s);
    }
}

fn draw_column(f: &mut Frame, app: &mut App, c: usize, ids: &[String], area: Rect) {
    app.col_rects.push((c, area));
    let tile_w = area.width.saturating_sub(1);
    let heights: Vec<u16> = ids.iter().map(|id| tile_height(app, id).min(area.height)).collect();

    // Scroll whole tiles so the selected one is visible.
    let mut scroll = app.col_scroll[c].min(ids.len() - 1);
    if app.follow_sel {
        if let Some(si) = ids.iter().position(|id| *id == app.sel.list) {
            scroll = scroll.min(si);
            while scroll < si && heights[scroll..=si].iter().sum::<u16>() > area.height {
                scroll += 1;
            }
        }
    }
    // Do not scroll past the point where the last tiles already fit.
    while scroll > 0 && heights[scroll - 1..].iter().sum::<u16>() <= area.height {
        scroll -= 1;
    }
    app.col_scroll[c] = scroll;

    let mut y = area.y;
    let mut last = scroll;
    for (i, id) in ids.iter().enumerate().skip(scroll) {
        let left = area.bottom().saturating_sub(y);
        if left < 2 {
            break;
        }
        let h = heights[i].min(left);
        draw_tile(f, app, id, Rect::new(area.x, y, tile_w, h));
        y += h;
        last = i;
    }
    if scroll > 0 {
        let mut s = Seg::default();
        s.button(&format!("▲ {scroll}"), Click::ColScroll(c, -1));
        place(f, app, area.x + tile_w.saturating_sub(s.w + 2), area.y, s.w, s);
    }
    let below = ids.len() - 1 - last;
    if below > 0 {
        let mut s = Seg::default();
        s.button(&format!("▼ {below}"), Click::ColScroll(c, 1));
        place(f, app, area.x + tile_w.saturating_sub(s.w + 2), area.bottom() - 1, s.w, s);
    }
}

fn draw_tile(f: &mut Frame, app: &mut App, id: &str, r: Rect) {
    let Some(list) = app.list(id).cloned() else { return };
    let selected = app.sel.list == id;
    let dragging = app.drag.as_ref().is_some_and(|d| d.moved && d.item == DragItem::Tile(id.to_string()));
    // Thick border when a dragged task would land in this tile as a top-level task.
    let task_target = app.task_drop.as_ref().is_some_and(|d| d.list == id && d.parent.is_none());
    let nest_target = app.task_drop.as_ref().filter(|d| d.list == id).and_then(|d| d.parent.clone());
    let tile_hover = hovered(app, r);
    let border = if task_target {
        ACCENT
    } else if dragging {
        SURFACE0
    } else if selected {
        ACCENT
    } else if tile_hover {
        OVERLAY
    } else {
        SURFACE1
    };
    f.render_widget(
        Block::bordered()
            .border_type(if task_target { BorderType::Thick } else { BorderType::Rounded })
            .border_style(Style::new().fg(border))
            .style(Style::new().bg(BASE).fg(TEXT)),
        r,
    );
    app.tile_rects.push((id.to_string(), r));
    // The whole top border is the drag handle; arrow, title and buttons sit on it.
    app.hits.push((Rect::new(r.x, r.y, r.width, 1), Click::DragTile(id.to_string())));

    let collapsed = app.is_collapsed(id);
    let today = date::today();
    let open = app.open_count(id);
    let overdue =
        app.list_tasks(id).iter().filter(|t| !t.done() && t.due_date().is_some_and(|d| d < today)).count();
    let w = r.width.saturating_sub(2) as usize;

    let mut right = Seg::default();
    if selected || tile_hover {
        right.buttons(vec![
            ("+", Click::AddTask(id.to_string())),
            ("✎", Click::RenameList(id.to_string())),
            ("✕", Click::DeleteList(id.to_string())),
        ]);
    }
    let mut head = Seg::default();
    head.click(
        if collapsed { " ▸ " } else { " ▾ " },
        Style::new().fg(ACCENT),
        Some(Style::new().bold()),
        Click::ToggleCollapse(id.to_string()),
    );
    let mut counts = Seg::default();
    if open > 0 {
        counts.text(format!(" {open}"), DIM);
    }
    if overdue > 0 {
        counts.text(format!(" ⚠{overdue}"), Style::new().fg(RED));
    }
    counts.text(" ", Style::new());
    let title_style = if selected && app.sel.row == 0 {
        Style::new().fg(CRUST).bg(ACCENT).bold()
    } else {
        Style::new().fg(if selected { ACCENT } else { TEXT }).bold()
    };
    let avail = w.saturating_sub((head.w + counts.w + right.w) as usize + 1);
    head.text(truncate(&list.title, avail), title_style);
    head.append(counts);
    place(f, app, r.x + 1, r.y, w as u16, head);
    if right.w > 0 {
        place(f, app, r.right().saturating_sub(right.w + 2), r.y, right.w, right);
    }

    if collapsed || r.height <= 2 {
        return;
    }
    let inner = Rect::new(r.x + 1, r.y + 1, r.width.saturating_sub(2), r.height - 2);
    let rows = app.tile_rows(id);
    let content = &rows[1..];
    let h = inner.height as usize;
    let mut ts = app.tile_scroll.get(id).copied().unwrap_or(0);
    if app.follow_sel && selected && app.sel.row > 0 {
        let k = app.sel.row - 1;
        if k < ts {
            ts = k;
        } else if k >= ts + h {
            ts = k + 1 - h;
        }
    }
    ts = ts.min(content.len().saturating_sub(h));
    app.tile_scroll.insert(id.to_string(), ts);

    let w = inner.width as usize;
    for (vi, row) in content.iter().enumerate().skip(ts).take(h) {
        let y = inner.y + (vi - ts) as u16;
        let row_rect = Rect::new(inner.x, y, inner.width, 1);
        let row_idx = vi + 1;
        let sel_row = selected && app.sel.row == row_idx;
        let active = sel_row || hovered(app, row_rect);
        let is_nest_target = matches!(*row, TileRow::Task { task, .. }
            if nest_target.as_deref() == Some(app.list_tasks(id)[task].id.as_str()));
        if is_nest_target {
            f.render_widget(Block::new().bg(SURFACE1), row_rect);
        } else if sel_row {
            f.render_widget(Block::new().bg(SELECTED_BG), row_rect);
        }
        app.hits.push((row_rect, Click::Select(id.to_string(), row_idx)));
        let seg = match *row {
            TileRow::Task { task, depth } => task_row(app, id, task, depth, w, active),
            _ => {
                let mut s = Seg::default();
                let style = if active { Style::new().fg(ACCENT) } else { DIM };
                s.click(" + Додати завдання", style, None, Click::AddTask(id.to_string()));
                s
            }
        };
        place(f, app, inner.x, y, inner.width, seg);
    }
    if ts > 0 {
        f.render_widget(Span::raw("▲").fg(ACCENT), Rect::new(r.right() - 1, inner.y, 1, 1));
    }
    if ts + h < content.len() {
        f.render_widget(Span::raw("▼").fg(ACCENT), Rect::new(r.right() - 1, inner.bottom() - 1, 1, 1));
    }
}

fn task_row(app: &App, list: &str, ti: usize, depth: u8, w: usize, active: bool) -> Seg {
    let t = &app.list_tasks(list)[ti];
    let ids = || (list.to_string(), t.id.clone());

    let mut right = Seg::default();
    let (due, due_style) = due_label(t);
    if !due.is_empty() {
        right.text(format!("{due} "), due_style);
    }
    if active {
        let (l, id) = ids();
        right.buttons(vec![
            ("+", Click::AddSubtask(l.clone(), id.clone())),
            ("◷", Click::EditDue(l.clone(), id.clone())),
            ("⇄", Click::MoveTask(l.clone(), id.clone())),
            ("✎", Click::EditTitle(l.clone(), id.clone())),
            ("✕", Click::DeleteTask(l, id)),
        ]);
    }

    let mut s = Seg::default();
    // Top-level tasks carry a fold arrow when they have subtasks.
    let subs = if depth == 0 { app.subtask_count(list, &t.id) } else { 0 };
    let folded = app.st.collapsed_tasks.contains(&t.id);
    if subs > 0 {
        s.text(" ", Style::new()).click(
            if folded { "▸ " } else { "▾ " },
            Style::new().fg(ACCENT),
            Some(Style::new().bold()),
            Click::ToggleSubtasks(t.id.clone()),
        );
    } else {
        s.text(format!("   {}", "   ".repeat(depth as usize)), Style::new());
    }
    let (mark, style) = if t.done() { ("✔", Style::new().fg(GREEN)) } else { ("○", DIM) };
    let (l, id) = ids();
    s.click(format!("{mark} "), style, Some(Style::new().fg(ACCENT).bold()), Click::ToggleDone(l, id));
    let note = if t.notes.is_some() { " ≡" } else { "" };
    let hidden = if folded && subs > 0 { format!(" +{subs}") } else { String::new() };
    let avail = w.saturating_sub((s.w + right.w) as usize + note.chars().count() + hidden.chars().count() + 1);
    let title_style = if t.done() { DIM.crossed_out() } else { Style::new() };
    s.text(truncate(&t.title, avail), title_style);
    s.text(note, DIM);
    s.text(hidden, Style::new().fg(ACCENT));
    let pad = w.saturating_sub((s.w + right.w) as usize);
    s.text(" ".repeat(pad), Style::new());
    s.append(right);
    s
}

/// Drop indicator and a ghost label following the cursor while a tile is dragged.
fn draw_drag(f: &mut Frame, app: &mut App) {
    let Some(d) = app.drag.as_ref().filter(|d| d.moved) else { return };
    let pos = d.pos;
    let title = match &d.item {
        DragItem::Tile(list) => app.list(list).map(|l| l.title.clone()).unwrap_or_default(),
        DragItem::Task { list, task } => {
            // Moving a task: the target tile is highlighted while drawing; only the ghost here.
            let name = app.list_tasks(list).iter().find(|t| t.id == *task).map(|t| t.title.clone());
            let to = match &app.task_drop {
                Some(d) => match app.nest_blocker(list, task, d.parent.as_deref()) {
                    Some(why) => format!(" ✕ {why}"),
                    None => match d.parent.as_deref() {
                        Some(p) => {
                            let p = app.list_tasks(&d.list).iter().find(|t| t.id == p).map(|t| t.title.clone());
                            format!(" → підзавдання «{}»", truncate(&p.unwrap_or_default(), 24))
                        }
                        None if d.list == *list => " → окреме завдання".into(),
                        None => format!(" → {}", app.list(&d.list).map(|l| l.title.as_str()).unwrap_or("")),
                    },
                },
                None => " → на завдання чи іншу плитку".into(),
            };
            let label = format!(" ⠿ {}{to} ", truncate(&name.unwrap_or_default(), 30));
            let area = f.area();
            let w = (label.chars().count() as u16).min(area.width);
            let x = (pos.x + 1).min(area.right().saturating_sub(w));
            f.render_widget(Span::styled(label, BTN_HOVER.bold()), Rect::new(x, pos.y, w, 1));
            return;
        }
    };
    let line = Style::new().fg(ACCENT).bold();
    let rect_of = |id: &str| app.tile_rects.iter().find(|(t, _)| t == id).map(|(_, r)| *r);
    match app.drop_target(pos) {
        Some(Drop::Above(t)) | Some(Drop::Below(t)) => {
            if let Some(r) = rect_of(&t) {
                let below = matches!(app.drop_target(pos), Some(Drop::Below(_)));
                let y = if below { r.bottom() - 1 } else { r.y };
                f.render_widget(Span::styled("━".repeat(r.width as usize), line), Rect::new(r.x, y, r.width, 1));
            }
        }
        Some(Drop::LeftOf(t)) | Some(Drop::RightOf(t)) => {
            if let Some(r) = rect_of(&t) {
                let right = matches!(app.drop_target(pos), Some(Drop::RightOf(_)));
                let x = if right { r.right() - 1 } else { r.x };
                for y in r.y..r.bottom() {
                    f.render_widget(Span::styled("┃", line), Rect::new(x, y, 1, 1));
                }
            }
        }
        Some(Drop::ColumnEnd(c)) => {
            let col = app.col_rects.iter().find(|(i, _)| *i == c).map(|(_, r)| *r);
            if let Some(col) = col {
                let bottom = app
                    .tile_rects
                    .iter()
                    .filter(|(_, r)| r.x == col.x)
                    .map(|(_, r)| r.bottom())
                    .max()
                    .unwrap_or(col.y);
                let y = bottom.min(col.bottom() - 1);
                let w = col.width.saturating_sub(1);
                f.render_widget(Span::styled("━".repeat(w as usize), line), Rect::new(col.x, y, w, 1));
            }
        }
        None => {}
    }
    let label = format!(" ⠿ {} ", truncate(&title, 30));
    let area = f.area();
    let w = (label.chars().count() as u16).min(area.width);
    let x = (pos.x + 1).min(area.right().saturating_sub(w));
    f.render_widget(Span::styled(label, BTN_HOVER.bold()), Rect::new(x, pos.y, w, 1));
}

fn draw_details(f: &mut Frame, app: &mut App, area: Rect) {
    let mut lines = Vec::new();
    let mut btns = Seg::default();
    let mut title = " Опис ";
    if let Some(t) = app.selected_task().cloned() {
        let l = app.sel.list.clone();
        let id = t.id.clone();
        btns.buttons(vec![
            (if t.done() { "↺ Відновити" } else { "✔ Готово" }, Click::ToggleDone(l.clone(), id.clone())),
            ("◷ Дедлайн", Click::EditDue(l.clone(), id.clone())),
            ("≡ Опис", Click::EditNotes(l.clone(), id.clone())),
            ("⇄ В інший список", Click::MoveTask(l.clone(), id.clone())),
        ]);
        lines.push(Line::raw(t.title.clone()).bold());
        if let Some(d) = t.due_date() {
            let today = date::today();
            let rel = match (d - today).num_days() {
                0 => " (сьогодні)".to_string(),
                1 => " (завтра)".to_string(),
                n if n < 0 => format!(" (прострочено на {} дн.)", -n),
                n => format!(" (через {n} дн.)"),
            };
            lines.push(Line::from(vec![Span::styled("Дедлайн: ", DIM), Span::raw(date::long(d) + &rel)]));
        }
        match &t.notes {
            Some(n) => lines.extend(n.lines().map(|l| Line::raw(l.to_string()))),
            None => lines.push(Line::styled("Без опису. Клікніть, щоб додати.", DIM)),
        }
        app.hits.push((area, Click::EditNotes(l, id)));
    } else if let Some(l) = app.sel_list().cloned() {
        title = " Список ";
        btns.buttons(vec![
            ("+ Завдання", Click::AddTask(l.id.clone())),
            ("✎ Назва", Click::RenameList(l.id.clone())),
            ("Очистити виконані", Click::DeleteCompleted(l.id.clone())),
        ]);
        lines.push(Line::raw(l.title.clone()).bold());
        lines.push(Line::styled(
            format!("Відкритих: {} · виконаних: {}", app.open_count(&l.id), app.done_count(&l.id)),
            DIM,
        ));
        lines.push(Line::styled("Тягніть плитку за заголовок, щоб переставити її.", DIM));
    }
    let p = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(block(title, false).style(Style::new().bg(BASE)));
    f.render_widget(p, area);
    let bx = area.x + area.width.saturating_sub(btns.w + 2);
    let min_x = area.x + title.chars().count() as u16 + 2;
    place(f, app, bx.max(min_x), area.y, area.right().saturating_sub(bx.max(min_x) + 1), btns);
}

fn draw_status(f: &mut Frame, app: &mut App, area: Rect) {
    let q = app.st.pending.len();
    let (text, style, click) = if app.auth_error.is_some() {
        (" ⚠ потрібен вхід — клікніть ".to_string(), Style::new().fg(RED).bold(), Click::Login)
    } else if app.net_error.is_some() {
        (format!(" ⚠ офлайн · {q} у черзі "), Style::new().fg(YELLOW), Click::Refresh)
    } else if app.busy {
        (" ⟳ синхронізація ".to_string(), Style::new().fg(SKY), Click::Refresh)
    } else if q > 0 {
        (format!(" … {q} у черзі "), Style::new().fg(SKY), Click::Refresh)
    } else if let Some(t) = app.last_sync {
        (format!(" ✓ {} ", t.format("%H:%M")), Style::new().fg(GREEN), Click::Refresh)
    } else {
        (" · ".to_string(), DIM, Click::Refresh)
    };
    let mut s = Seg::default();
    s.click(text, style, Some(Style::new().add_modifier(Modifier::UNDERLINED)), click);
    if let Some((m, err, at)) = &app.flash {
        if at.elapsed().as_secs() < 4 {
            s.text(format!(" {m}"), if *err { Style::new().fg(RED) } else { Style::new() });
        }
    }
    let mut help = Seg::default();
    help.click(" ? довідка ", DIM, Some(Style::new().fg(ACCENT)), Click::Help);
    let hx = area.right().saturating_sub(help.w);
    place(f, app, area.x, area.y, hx.saturating_sub(area.x), s);
    place(f, app, hx, area.y, help.w, help);
}

fn popup(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(2));
    let h = h.min(area.height.saturating_sub(2));
    let [r] = Layout::horizontal([Constraint::Length(w)]).flex(Flex::Center).areas(area);
    let [r] = Layout::vertical([Constraint::Length(h)]).flex(Flex::Center).areas(r);
    r
}

/// Soft-wraps the editor text and returns visual lines plus the cursor's (row, col).
fn wrap_editor(chars: &[char], cur: usize, width: usize) -> (Vec<String>, (usize, usize)) {
    let width = width.max(1);
    let mut lines = vec![String::new()];
    let mut col = 0;
    let mut pos = (0, 0);
    for (i, &c) in chars.iter().enumerate() {
        if i == cur {
            pos = (lines.len() - 1, col);
        }
        if c == '\n' {
            lines.push(String::new());
            col = 0;
            continue;
        }
        if col == width {
            lines.push(String::new());
            col = 0;
            if i == cur {
                pos = (lines.len() - 1, 0);
            }
        }
        lines.last_mut().unwrap().push(c);
        col += 1;
    }
    if cur >= chars.len() {
        if col == width {
            lines.push(String::new());
            col = 0;
        }
        pos = (lines.len() - 1, col);
    }
    (lines, pos)
}

fn draw_input(f: &mut Frame, app: &mut App) {
    let Mode::Input { kind, ed } = &app.mode else { return };
    let title = match kind {
        InputKind::NewTask { parent: None, .. } => " Нове завдання ",
        InputKind::NewTask { .. } => " Нове підзавдання ",
        InputKind::Title { .. } => " Назва ",
        InputKind::Notes { .. } => " Опис · Enter — новий рядок, Ctrl+S — зберегти ",
        InputKind::Due { .. } => " Дедлайн · завтра, +3, пт, 25.10 ",
        InputKind::NewList => " Новий список ",
        InputKind::RenameList { .. } => " Назва списку ",
    };
    let is_due = matches!(kind, InputKind::Due { .. });
    let area = app.panel;
    let h = if kind.multiline() {
        area.height.saturating_sub(4).clamp(5, 14)
    } else if is_due {
        4
    } else {
        3
    };
    let r = popup(area, 72, h);
    let text_h = if is_due { 1 } else { r.height.saturating_sub(2) as usize };
    let inner_w = r.width.saturating_sub(2) as usize;
    let (lines, (row, col)) = wrap_editor(&ed.chars, ed.cur, inner_w.saturating_sub(1));
    let scroll = row.saturating_sub(text_h.saturating_sub(1));
    surface(f, r, BASE);
    f.render_widget(block(title, true), r);
    let text_area = Rect::new(r.x + 1, r.y + 1, r.width.saturating_sub(2), text_h as u16);
    let p = Paragraph::new(lines.into_iter().map(Line::raw).collect::<Vec<_>>()).scroll((scroll as u16, 0));
    f.render_widget(p, text_area);
    f.set_cursor_position((r.x + 1 + col as u16, r.y + 1 + (row - scroll) as u16));

    if is_due {
        let mut presets = Seg::default();
        presets.buttons(vec![
            ("Сьогодні", Click::DuePreset("t")),
            ("Завтра", Click::DuePreset("tm")),
            ("+3 дні", Click::DuePreset("+3")),
            ("Пн", Click::DuePreset("пн")),
            ("+1 тиж", Click::DuePreset("+1w")),
            ("Без дати", Click::DuePreset("-")),
        ]);
        place(f, app, r.x + 1, r.y + 2, r.width.saturating_sub(2), presets);
    }
    let mut btns = Seg::default();
    btns.buttons(vec![("Зберегти", Click::Save), ("Скасувати", Click::Cancel)]);
    let bx = r.right().saturating_sub(btns.w + 2);
    place(f, app, bx, r.bottom() - 1, btns.w, btns);
}

fn draw_confirm(f: &mut Frame, app: &mut App, msg: &str) {
    let r = popup(app.panel, 56, 6);
    surface(f, r, BASE);
    let p = Paragraph::new(msg.to_string())
        .wrap(Wrap { trim: true })
        .block(block(" Підтвердження ", true).border_style(Style::new().fg(RED)));
    f.render_widget(p, r);
    let mut btns = Seg::default();
    btns.button("  Так  ", Click::Yes).text("  ", Style::new()).button("  Ні  ", Click::No);
    let bx = r.x + r.width.saturating_sub(btns.w) / 2;
    place(f, app, bx, r.bottom().saturating_sub(2), btns.w, btns);
}

fn draw_move(f: &mut Frame, app: &mut App, list: &str, task: &str, sel: usize) {
    let targets = app.move_targets(list);
    let name = app.list_tasks(list).iter().find(|t| t.id == task).map(|t| t.title.clone()).unwrap_or_default();
    let r = popup(app.panel, 50, targets.len() as u16 + 3);
    surface(f, r, BASE);
    f.render_widget(block(format!(" Перенести «{}» ", truncate(&name, 30)), true), r);
    let w = r.width.saturating_sub(2);
    for (i, id) in targets.iter().enumerate() {
        let y = r.y + 1 + i as u16;
        if y >= r.bottom() - 2 {
            break;
        }
        let row = Rect::new(r.x + 1, y, w, 1);
        let active = i == sel || hovered(app, row);
        if active {
            f.render_widget(Block::new().bg(SELECTED_BG), row);
        }
        let title = app.list(id).map(|l| l.title.clone()).unwrap_or_default();
        let mut s = Seg::default();
        s.text(format!(" {} ", if i < 9 { (i + 1).to_string() } else { " ".into() }), Style::new().fg(ACCENT).bold());
        s.text(truncate(&title, w as usize - 8), if active { Style::new().fg(ACCENT).bold() } else { Style::new() });
        s.text(format!(" {}", app.open_count(id)), DIM);
        app.hits.push((row, Click::MoveTo(id.clone())));
        place(f, app, row.x, y, w, s);
    }
    let mut btns = Seg::default();
    btns.button("Скасувати", Click::Cancel);
    place(f, app, r.right().saturating_sub(btns.w + 2), r.bottom() - 1, btns.w, btns);
}

fn draw_help(f: &mut Frame) {
    const KEYS: &[(&str, &str)] = &[
        ("Мишею", "кнопки, ○ — виконати, ▾ — згорнути"),
        ("", "список чи підзавдання"),
        ("", "тягніть плитку за заголовок: на край"),
        ("", "іншої — ліворуч/праворуч, на середину —"),
        ("", "вище/нижче; подвійний клік — редагувати"),
        ("", "клік повз панель — сховати"),
        ("j k ↑ ↓", "рух по плитках"),
        ("h l ← →", "сусідня колонка"),
        ("H J K L", "перемістити плитку (або Shift+стрілки)"),
        ("Tab [ ]", "наступна / попередня плитка"),
        ("Enter", "згорнути список / змінити назву"),
        ("z Z", "згорнути / розгорнути всі"),
        ("a  A", "нове завдання / підзавдання"),
        ("N", "новий список"),
        ("Space", "виконано / не виконано"),
        ("e  i  d", "назва · опис · дедлайн"),
        ("s  ▾", "сховати / показати підзавдання"),
        ("m  ⇄", "перенести в інший список"),
        ("", "тягніть завдання: на інше — стане"),
        ("", "підзавданням, на плитку — окремим"),
        ("x  Del", "видалити"),
        ("c / C", "показати виконані / видалити їх"),
        ("r", "синхронізувати зараз"),
        ("Ctrl+L", "увійти в Google знову"),
        ("q  Esc", "сховати (у кишеню)"),
        ("Q", "вийти"),
    ];
    let lines: Vec<Line> = KEYS
        .iter()
        .map(|(k, d)| Line::from(vec![Span::raw(format!(" {k:<9}")).fg(ACCENT).bold(), Span::raw(*d)]))
        .collect();
    let r = popup(f.area(), 58, KEYS.len() as u16 + 2);
    surface(f, r, BASE);
    f.render_widget(Paragraph::new(lines).block(block(" Довідка · клік — закрити ", true)), r);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Sel;
    use crate::model::{Op, State, TaskList};
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn sample() -> App {
        let mut st = State::default();
        for (id, title) in [("a", "Мої завдання"), ("b", "Робота"), ("c", "Покупки")] {
            st.lists.push(TaskList { id: id.into(), title: title.into() });
        }
        let task = |id: &str, title: &str, parent: Option<&str>| Task {
            id: id.into(),
            title: title.into(),
            status: "needsAction".into(),
            parent: parent.map(String::from),
            position: id.into(),
            ..Default::default()
        };
        st.tasks.insert(
            "a".into(),
            vec![task("1", "Купити молоко", None), task("2", "Знежирене", Some("1")), task("3", "Подзвонити", None)],
        );
        st.tasks.insert("b".into(), vec![task("4", "Звіт", None)]);
        st.tasks.insert("c".into(), vec![task("5", "Хліб", None)]);
        st.collapsed.insert("b".into());
        App::new(st, std::sync::mpsc::channel().0, true)
    }

    fn render(app: &mut App) -> String {
        let mut term = Terminal::new(TestBackend::new(130, 36)).unwrap();
        term.draw(|f| draw(f, app)).unwrap();
        let buf = term.backend().buffer();
        (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>().trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn mouse(app: &mut App, kind: MouseEventKind, r: Rect) {
        app.on_mouse(MouseEvent { kind, column: r.x, row: r.y, modifiers: KeyModifiers::NONE });
    }

    fn hit(app: &App, c: Click) -> Rect {
        app.hits.iter().rev().find(|(_, h)| *h == c).map(|(r, _)| *r).unwrap_or_else(|| panic!("no {c:?}"))
    }

    fn press(app: &mut App, c: Click) {
        let r = hit(app, c);
        mouse(app, MouseEventKind::Down(MouseButton::Left), r);
    }

    fn tile(app: &App, id: &str) -> Rect {
        app.tile_rects.iter().find(|(t, _)| t == id).unwrap().1
    }

    fn drag(app: &mut App, list: &str, to: Rect) {
        let from = hit(app, Click::DragTile(list.into()));
        let from = Rect::new(from.x + 20, from.y, 1, 1);
        mouse(app, MouseEventKind::Down(MouseButton::Left), from);
        mouse(app, MouseEventKind::Drag(MouseButton::Left), to);
        render(app);
        mouse(app, MouseEventKind::Up(MouseButton::Left), to);
        render(app);
    }

    #[test]
    fn tiles_render_side_by_side() {
        let mut app = sample();
        app.sel = Sel { list: "a".into(), row: 1 };
        let screen = render(&mut app);
        println!("{screen}");
        assert_eq!(app.st.layout, vec![vec!["a"], vec!["b"], vec!["c"]]);
        let line = screen.lines().find(|l| l.contains("Мої завдання")).unwrap();
        assert!(line.contains("Робота") && line.contains("Покупки"), "tiles share a row: {line}");
        assert!(screen.contains("Знежирене"));
        assert!(!screen.contains("Звіт"), "collapsed tile hides its tasks");
        assert!(app.hits.iter().any(|(_, c)| *c == Click::DeleteTask("a".into(), "1".into())));
    }

    #[test]
    fn mouse_actions() {
        let mut app = sample();
        render(&mut app);
        press(&mut app, Click::ToggleCollapse("b".into()));
        assert!(!app.is_collapsed("b"));
        assert!(render(&mut app).contains("Звіт"));

        press(&mut app, Click::ToggleDone("a".into(), "3".into()));
        assert!(app.list_tasks("a").iter().find(|t| t.id == "3").unwrap().done());
        assert_eq!(app.st.pending.len(), 1);

        app.sel = Sel { list: "a".into(), row: 1 };
        render(&mut app);
        press(&mut app, Click::DeleteTask("a".into(), "1".into()));
        render(&mut app);
        let yes = app.hits[app.modal_from..].iter().find(|(_, c)| *c == Click::Yes).unwrap().0;
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), yes);
        assert!(app.list_tasks("a").iter().all(|t| t.id != "1" && t.id != "2"));

        app.sel = Sel { list: "c".into(), row: 1 };
        render(&mut app);
        press(&mut app, Click::EditDue("c".into(), "5".into()));
        let screen = render(&mut app);
        println!("{screen}");
        let tm = app.hits[app.modal_from..].iter().find(|(_, c)| *c == Click::DuePreset("tm")).unwrap().0;
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), tm);
        assert_eq!(app.list_tasks("c")[0].due_date(), Some(date::today() + chrono::Duration::days(1)));
    }

    #[test]
    fn drag_tiles_around() {
        let mut app = sample();
        render(&mut app);
        // Middle-bottom of "Мої завдання": "Покупки" goes below it.
        let a = tile(&app, "a");
        drag(&mut app, "c", Rect::new(a.x + a.width / 2, a.bottom() - 1, 1, 1));
        assert_eq!(app.st.layout, vec![vec!["a", "c"], vec!["b"]]);
        println!("{}", render(&mut app));

        // Middle-top of "Мої завдання": "Покупки" goes above it.
        let a = tile(&app, "a");
        drag(&mut app, "c", Rect::new(a.x + a.width / 2, a.y, 1, 1));
        assert_eq!(app.st.layout, vec![vec!["c", "a"], vec!["b"]]);

        // Right edge of "Робота": a new column to its right.
        let b = tile(&app, "b");
        drag(&mut app, "a", Rect::new(b.right() - 1, b.y + 1, 1, 1));
        assert_eq!(app.st.layout, vec![vec!["c"], vec!["b"], vec!["a"]]);

        // Left edge of "Покупки": a new first column.
        let c = tile(&app, "c");
        drag(&mut app, "b", Rect::new(c.x, c.y, 1, 1));
        assert_eq!(app.st.layout, vec![vec!["b"], vec!["c"], vec!["a"]]);

        // Empty space under a column: appended to it.
        let col = app.col_rects.iter().find(|(c, _)| *c == 0).unwrap().1;
        drag(&mut app, "a", Rect::new(col.x + 5, col.bottom() - 1, 1, 1));
        assert_eq!(app.st.layout, vec![vec!["b", "a"], vec!["c"]]);

        // A click without moving only selects.
        let before = app.st.layout.clone();
        let r = hit(&app, Click::DragTile("c".into()));
        drag(&mut app, "c", Rect::new(r.x + 20, r.y, 1, 1));
        assert_eq!(app.st.layout, before);
        assert_eq!(app.sel, Sel { list: "c".into(), row: 0 });
    }

    #[test]
    fn move_tasks_between_lists() {
        let mut app = sample();
        render(&mut app);
        // Drag "Купити молоко" (with its subtask) onto the "Покупки" tile.
        let row = hit(&app, Click::Select("a".into(), 1));
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), Rect::new(row.x + 10, row.y, 1, 1));
        // Onto the "+ Додати завдання" row: a top-level task of that list.
        let add = hit(&app, Click::Select("c".into(), 2));
        let to = Rect::new(add.x + add.width / 2, add.y, 1, 1);
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), to);
        let screen = render(&mut app);
        println!("{screen}");
        assert_eq!(app.task_drop, Some(crate::app::TaskDrop { list: "c".into(), parent: None }));
        assert!(screen.contains("→ Покупки"));
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), to);
        let ids = |app: &App, l: &str| app.list_tasks(l).iter().map(|t| t.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&app, "a"), ["3"]);
        assert_eq!(ids(&app, "c"), ["5", "1", "2"]);
        assert_eq!(app.list_tasks("c")[2].parent.as_deref(), Some("1"));
        assert!(matches!(&app.st.pending[0], Op::MoveTask { list, task, dest, parent: None } if list == "a" && task == "1" && dest == "c"));
        assert_eq!(app.selected_task().map(|t| t.id.as_str()), Some("1"));

        // Dropping back on its own tile does nothing.
        render(&mut app);
        let row = hit(&app, Click::Select("c".into(), 1));
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), Rect::new(row.x + 10, row.y, 1, 1));
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), Rect::new(row.x + 8, row.y + 1, 1, 1));
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), Rect::new(row.x + 8, row.y + 1, 1, 1));
        assert_eq!(app.st.pending.len(), 1);

        // Subtask via keyboard picker: becomes a top-level task in "Робота".
        app.sel = Sel { list: "c".into(), row: 0 };
        app.select_task_pub("c", "2");
        app.on_key(crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Char('m'), KeyModifiers::NONE));
        let screen = render(&mut app);
        println!("{screen}");
        assert!(matches!(app.mode, Mode::MoveTo { .. }));
        // Targets in board order without "Покупки": Мої завдання, Робота.
        let robota = app.hits[app.modal_from..].iter().find(|(_, c)| *c == Click::MoveTo("b".into())).unwrap().0;
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), robota);
        let moved = app.list_tasks("b").iter().find(|t| t.id == "2").unwrap();
        assert_eq!(moved.parent, None);
        assert!(app.list_tasks("c").iter().all(|t| t.id != "2"));

        // "Робота" is collapsed, so the selection falls back to its header.
        assert_eq!(app.sel, Sel { list: "b".into(), row: 0 });

        // Keyboard: m then 1 sends it to the first other list.
        app.st.collapsed.clear();
        assert!(app.select_task_pub("b", "2"));
        app.on_key(crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Char('m'), KeyModifiers::NONE));
        app.on_key(crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Char('1'), KeyModifiers::NONE));
        assert!(app.list_tasks("a").iter().any(|t| t.id == "2"));
    }

    fn drag_task(app: &mut App, list: &str, row: usize, to: Rect) -> String {
        render(app);
        let from = hit(app, Click::Select(list.into(), row));
        mouse(app, MouseEventKind::Down(MouseButton::Left), Rect::new(from.x + 10, from.y, 1, 1));
        mouse(app, MouseEventKind::Drag(MouseButton::Left), to);
        let screen = render(app);
        mouse(app, MouseEventKind::Up(MouseButton::Left), to);
        screen
    }

    fn parent_of<'a>(app: &'a App, list: &str, id: &str) -> Option<&'a str> {
        app.list_tasks(list).iter().find(|t| t.id == id).and_then(|t| t.parent.as_deref())
    }

    #[test]
    fn drop_task_on_task_nests_it() {
        let mut app = sample();
        render(&mut app);
        // "Подзвонити" (row 3) onto "Купити молоко" (row 1) in the same list.
        let target = hit(&app, Click::Select("a".into(), 1));
        let screen = drag_task(&mut app, "a", 3, Rect::new(target.x + 10, target.y, 1, 1));
        assert!(screen.contains("→ підзавдання «Купити молоко»"), "{screen}");
        assert_eq!(parent_of(&app, "a", "3"), Some("1"));
        assert!(matches!(&app.st.pending[0],
            Op::MoveTask { list, task, dest, parent: Some(p) } if list == "a" && task == "3" && dest == "a" && p == "1"));

        // Onto a subtask ("Знежирене", parent "1"): lands under that subtask's parent.
        let rows = app.tile_rows("c");
        assert_eq!(rows.len(), 3);
        render(&mut app);
        let sub = hit(&app, Click::Select("a".into(), 2));
        drag_task(&mut app, "c", 1, Rect::new(sub.x + 10, sub.y, 1, 1));
        assert_eq!(parent_of(&app, "a", "5"), Some("1"), "cross-list nesting");
        assert!(app.list_tasks("c").is_empty());

        // A task that has subtasks cannot be nested.
        app.st.collapsed.clear();
        render(&mut app);
        let other = hit(&app, Click::Select("b".into(), 1));
        let screen = drag_task(&mut app, "a", 1, Rect::new(other.x + 10, other.y, 1, 1));
        assert!(screen.contains("✕ завдання з підзавданнями не можна вкласти"), "{screen}");
        assert_eq!(parent_of(&app, "b", "1"), None);
        assert!(app.list_tasks("a").iter().any(|t| t.id == "1"));

        // A subtask dropped on the rest of its own tile becomes a top-level task again.
        render(&mut app);
        let add = app
            .hits
            .iter()
            .find(|(_, c)| matches!(c, Click::Select(l, r) if l == "a" && app.tile_rows("a").get(*r) == Some(&TileRow::Add)))
            .unwrap()
            .0;
        let row = app.tile_rows("a").iter().position(|r| matches!(r, TileRow::Task { task, .. } if app.list_tasks("a")[*task].id == "3")).unwrap();
        let screen = drag_task(&mut app, "a", row, Rect::new(add.x + 10, add.y, 1, 1));
        assert!(screen.contains("→ окреме завдання"), "{screen}");
        assert_eq!(parent_of(&app, "a", "3"), None);
    }

    #[test]
    fn fold_subtasks() {
        let mut app = sample();
        app.sel = Sel { list: "a".into(), row: 2 }; // "Знежирене", subtask of "1"
        let screen = render(&mut app);
        assert!(screen.contains("▾ ○ Купи"), "{screen}");
        press(&mut app, Click::ToggleSubtasks("1".into()));
        let screen = render(&mut app);
        println!("{screen}");
        assert!(!screen.contains("Знежирене"));
        assert!(screen.contains("▸ ○ Купи… +1"), "{screen}");
        assert_eq!(app.selected_task().map(|t| t.id.as_str()), Some("1"), "selection moves to the parent");
        assert!(app.st.collapsed_tasks.contains("1"));

        // `s` unfolds again, from the parent or from a subtask.
        app.on_key(crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Char('s'), KeyModifiers::NONE));
        assert!(render(&mut app).contains("Знежирене"));
        app.sel = Sel { list: "a".into(), row: 2 };
        app.on_key(crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Char('s'), KeyModifiers::NONE));
        assert!(!render(&mut app).contains("Знежирене"));
        // Tasks without subtasks get no arrow and ignore `s`.
        app.select_task_pub("a", "3");
        app.on_key(crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Char('s'), KeyModifiers::NONE));
        assert!(!app.st.collapsed_tasks.contains("3"));
    }

    #[test]
    fn keyboard_moves_tiles() {
        let mut app = sample();
        app.sel = Sel { list: "c".into(), row: 0 };
        let key = |app: &mut App, c: char| {
            app.on_key(crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Char(c), KeyModifiers::SHIFT))
        };
        key(&mut app, 'H');
        assert_eq!(app.st.layout, vec![vec!["a"], vec!["c", "b"]]);
        key(&mut app, 'J');
        assert_eq!(app.st.layout, vec![vec!["a"], vec!["b", "c"]]);
        key(&mut app, 'L');
        assert_eq!(app.st.layout, vec![vec!["a"], vec!["b"], vec!["c"]]);
        key(&mut app, 'L');
        assert_eq!(app.st.layout, vec![vec!["a"], vec!["b"], vec!["c"]], "lone tile at the edge stays");
    }

    #[test]
    fn wraps_and_places_cursor() {
        let c: Vec<char> = "abcdef\nxy".chars().collect();
        let (lines, pos) = wrap_editor(&c, 9, 4);
        assert_eq!(lines, vec!["abcd", "ef", "xy"]);
        assert_eq!(pos, (2, 2));
        let (_, pos) = wrap_editor(&c, 4, 4);
        assert_eq!(pos, (1, 0));
    }
}
