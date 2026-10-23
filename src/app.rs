use crate::api::ApiError;
use crate::date;
use crate::model::{Op, State, Task, TaskList};
use crate::sync::{Created, Req, Resp};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

const AUTO_REFRESH: Duration = Duration::from_secs(120);
const FOCUS_REFRESH: Duration = Duration::from_secs(10);
const RETRY: Duration = Duration::from_secs(15);
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// New lists open in their own column until there are this many columns.
const AUTO_COLUMNS: usize = 3;

/// One line inside a list tile.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum TileRow {
    Header,
    Task { task: usize, depth: u8 },
    /// "+ Додати завдання" at the end of an expanded tile.
    Add,
}

/// Selected tile and the row inside it (0 is the header).
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Sel {
    pub list: String,
    pub row: usize,
}

/// Where a dragged tile lands, relative to another tile or column.
#[derive(Clone, PartialEq, Debug)]
pub enum Drop {
    Above(String),
    Below(String),
    LeftOf(String),
    RightOf(String),
    ColumnEnd(usize),
}

pub struct Drag {
    pub list: String,
    pub from: Position,
    pub pos: Position,
    pub moved: bool,
}

/// Something a mouse click can trigger; regions are registered while drawing.
#[derive(Clone, PartialEq, Debug)]
pub enum Click {
    Select(String, usize),
    /// Tile header: click selects, drag moves the tile.
    DragTile(String),
    ToggleCollapse(String),
    ToggleDone(String, String),
    AddTask(String),
    AddSubtask(String, String),
    EditTitle(String, String),
    EditNotes(String, String),
    EditDue(String, String),
    DeleteTask(String, String),
    DeleteCompleted(String),
    AddList,
    RenameList(String),
    DeleteList(String),
    ExpandAll(bool),
    ColScroll(usize, i32),
    ToggleCompleted,
    Refresh,
    Login,
    Help,
    Hide,
    Save,
    Cancel,
    Yes,
    No,
    DuePreset(&'static str),
}

#[derive(Clone)]
pub enum InputKind {
    NewTask { list: String, parent: Option<String> },
    Title { list: String, task: String },
    Notes { list: String, task: String },
    Due { list: String, task: String },
    NewList,
    RenameList { list: String },
}

impl InputKind {
    pub fn multiline(&self) -> bool {
        matches!(self, InputKind::Notes { .. })
    }
}

pub enum Confirm {
    DeleteTask { list: String, task: String },
    DeleteList { list: String },
    DeleteCompleted { list: String },
}

pub enum Mode {
    Normal,
    Input { kind: InputKind, ed: Editor },
    Confirm { what: Confirm, msg: String },
    Help,
}

/// Tiny text editor for popups; works on chars so Cyrillic is safe.
#[derive(Default)]
pub struct Editor {
    pub chars: Vec<char>,
    pub cur: usize,
}

impl Editor {
    pub fn new(s: &str) -> Self {
        let chars: Vec<char> = s.chars().collect();
        Self { cur: chars.len(), chars }
    }
    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }
    fn line_start(&self, at: usize) -> usize {
        self.chars[..at].iter().rposition(|&c| c == '\n').map_or(0, |i| i + 1)
    }
    fn line_end(&self, at: usize) -> usize {
        self.chars[at..].iter().position(|&c| c == '\n').map_or(self.chars.len(), |i| at + i)
    }
    fn insert(&mut self, c: char) {
        self.chars.insert(self.cur, c);
        self.cur += 1;
    }
    fn up(&mut self) {
        let s = self.line_start(self.cur);
        if s == 0 {
            return;
        }
        let col = self.cur - s;
        let ps = self.line_start(s - 1);
        self.cur = ps + col.min(s - 1 - ps);
    }
    fn down(&mut self) {
        let e = self.line_end(self.cur);
        if e == self.chars.len() {
            return;
        }
        let col = self.cur - self.line_start(self.cur);
        let ne = self.line_end(e + 1);
        self.cur = (e + 1 + col).min(ne);
    }
    fn delete_word_back(&mut self) {
        let mut i = self.cur;
        while i > 0 && self.chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !self.chars[i - 1].is_whitespace() {
            i -= 1;
        }
        self.chars.drain(i..self.cur);
        self.cur = i;
    }

    /// Returns true when the key was consumed.
    fn key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Char('a') if ctrl => self.cur = self.line_start(self.cur),
            KeyCode::Char('e') if ctrl => self.cur = self.line_end(self.cur),
            KeyCode::Char('w') if ctrl => self.delete_word_back(),
            KeyCode::Backspace if ctrl || k.modifiers.contains(KeyModifiers::ALT) => self.delete_word_back(),
            KeyCode::Char('u') if ctrl => {
                let s = self.line_start(self.cur);
                self.chars.drain(s..self.cur);
                self.cur = s;
            }
            KeyCode::Char(c) if !ctrl => self.insert(c),
            KeyCode::Backspace if self.cur > 0 => {
                self.cur -= 1;
                self.chars.remove(self.cur);
            }
            KeyCode::Delete if self.cur < self.chars.len() => {
                self.chars.remove(self.cur);
            }
            KeyCode::Left => self.cur = self.cur.saturating_sub(1),
            KeyCode::Right => self.cur = (self.cur + 1).min(self.chars.len()),
            KeyCode::Home => self.cur = self.line_start(self.cur),
            KeyCode::End => self.cur = self.line_end(self.cur),
            KeyCode::Up => self.up(),
            KeyCode::Down => self.down(),
            _ => return false,
        }
        true
    }
}

/// Lets hotkeys work while the Ukrainian layout is active.
fn latin(c: char) -> char {
    const UA: &str = "йцукенгшщзхїфівапролджєячсмитьбюЙЦУКЕНГШЩЗХЇФІВАПРОЛДЖЄЯЧСМИТЬБЮ";
    const EN: &str = "qwertyuiop[]asdfghjkl;'zxcvbnm,.QWERTYUIOP{}ASDFGHJKL:\"ZXCVBNM<>";
    UA.chars().position(|u| u == c).and_then(|i| EN.chars().nth(i)).unwrap_or(c)
}

pub struct App {
    pub st: State,
    pub mode: Mode,
    pub sel: Sel,
    /// Set by navigation so the next frame scrolls the selection into view.
    pub follow_sel: bool,
    /// First visible tile per column, first visible task row per tile, first visible column.
    pub col_scroll: Vec<usize>,
    pub tile_scroll: HashMap<String, usize>,
    pub col_offset: usize,
    /// Geometry of the last frame, for hit testing and drag & drop.
    pub hits: Vec<(Rect, Click)>,
    pub tile_rects: Vec<(String, Rect)>,
    pub col_rects: Vec<(usize, Rect)>,
    pub panel: Rect,
    /// Index in `hits` where the open popup's regions start.
    pub modal_from: usize,
    pub hover: Option<Position>,
    pub drag: Option<Drag>,
    pub busy: bool,
    pub net_error: Option<String>,
    pub auth_error: Option<String>,
    pub last_sync: Option<chrono::DateTime<chrono::Local>>,
    pub flash: Option<(String, bool, Instant)>,
    pub quit: bool,
    pub login_requested: bool,
    pub pocket: bool,
    tx: Sender<Req>,
    last_click: Option<(Instant, Click)>,
    want_refresh: bool,
    last_refresh: Option<Instant>,
    retry_at: Option<Instant>,
    tmp_seq: u64,
    dirty: bool,
}

impl App {
    pub fn new(st: State, tx: Sender<Req>, pocket: bool) -> Self {
        let mut app = Self {
            st,
            mode: Mode::Normal,
            sel: Sel::default(),
            follow_sel: true,
            col_scroll: Vec::new(),
            tile_scroll: HashMap::new(),
            col_offset: 0,
            hits: Vec::new(),
            tile_rects: Vec::new(),
            col_rects: Vec::new(),
            panel: Rect::default(),
            modal_from: 0,
            hover: None,
            drag: None,
            busy: false,
            net_error: None,
            auth_error: None,
            last_sync: None,
            flash: None,
            quit: false,
            login_requested: false,
            pocket,
            tx,
            last_click: None,
            want_refresh: true,
            last_refresh: None,
            retry_at: None,
            tmp_seq: 0,
            dirty: false,
        };
        app.normalize_layout();
        app.clamp();
        app
    }

    // ---------- queries ----------

    pub fn list(&self, id: &str) -> Option<&TaskList> {
        self.st.lists.iter().find(|l| l.id == id)
    }

    pub fn list_tasks(&self, list: &str) -> &[Task] {
        self.st.tasks.get(list).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn open_count(&self, list: &str) -> usize {
        self.list_tasks(list).iter().filter(|t| !t.done()).count()
    }

    pub fn done_count(&self, list: &str) -> usize {
        self.list_tasks(list).iter().filter(|t| t.done()).count()
    }

    pub fn is_collapsed(&self, list: &str) -> bool {
        self.st.collapsed.contains(list)
    }

    /// Tasks of one list as (index into its vec, depth): open tasks by position with
    /// their subtasks, then completed ones (newest first).
    pub fn task_rows(&self, list: &str) -> Vec<(usize, u8)> {
        let tasks = self.list_tasks(list);
        let ids: HashSet<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
        let show_done = self.st.show_completed;
        let children = |pid: &str| {
            let mut c: Vec<usize> = (0..tasks.len())
                .filter(|&i| tasks[i].parent.as_deref() == Some(pid) && (show_done || !tasks[i].done()))
                .collect();
            c.sort_by(|&a, &b| (tasks[a].done(), &tasks[a].position).cmp(&(tasks[b].done(), &tasks[b].position)));
            c
        };
        let top: Vec<usize> = (0..tasks.len())
            .filter(|&i| tasks[i].parent.as_deref().is_none_or(|p| !ids.contains(p)))
            .collect();
        let mut open: Vec<usize> = top.iter().copied().filter(|&i| !tasks[i].done()).collect();
        open.sort_by(|&a, &b| tasks[a].position.cmp(&tasks[b].position));
        let mut done: Vec<usize> =
            if show_done { top.iter().copied().filter(|&i| tasks[i].done()).collect() } else { vec![] };
        done.sort_by(|&a, &b| tasks[b].completed.cmp(&tasks[a].completed));

        let mut out = Vec::new();
        for i in open.into_iter().chain(done) {
            out.push((i, 0));
            out.extend(children(&tasks[i].id).into_iter().map(|c| (c, 1)));
        }
        out
    }

    /// Lines of a tile: header, then (unless collapsed) tasks and the add row.
    pub fn tile_rows(&self, list: &str) -> Vec<TileRow> {
        let mut out = vec![TileRow::Header];
        if !self.is_collapsed(list) {
            out.extend(self.task_rows(list).into_iter().map(|(task, depth)| TileRow::Task { task, depth }));
            out.push(TileRow::Add);
        }
        out
    }

    pub fn sel_row(&self) -> Option<TileRow> {
        self.tile_rows(&self.sel.list).get(self.sel.row).copied()
    }

    pub fn sel_list(&self) -> Option<&TaskList> {
        self.list(&self.sel.list)
    }

    pub fn selected_task(&self) -> Option<&Task> {
        match self.sel_row()? {
            TileRow::Task { task, .. } => self.list_tasks(&self.sel.list).get(task),
            _ => None,
        }
    }

    fn selected_ids(&self) -> Option<(String, String)> {
        Some((self.sel.list.clone(), self.selected_task()?.id.clone()))
    }

    fn task_mut(&mut self, list: &str, id: &str) -> Option<&mut Task> {
        self.st.tasks.get_mut(list)?.iter_mut().find(|t| t.id == id)
    }

    /// (column, index in column) of a tile.
    pub fn tile_pos(&self, list: &str) -> Option<(usize, usize)> {
        self.st
            .layout
            .iter()
            .enumerate()
            .find_map(|(c, col)| col.iter().position(|id| id == list).map(|i| (c, i)))
    }

    // ---------- helpers ----------

    pub fn flash(&mut self, msg: impl Into<String>, error: bool) {
        self.flash = Some((msg.into(), error, Instant::now()));
    }

    fn tmp_id(&mut self) -> String {
        self.tmp_seq += 1;
        let ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("tmp-{ns:x}-{}", self.tmp_seq)
    }

    fn push(&mut self, op: Op) {
        self.st.pending.push(op);
        self.retry_at = None;
        self.dirty = true;
    }

    /// Drops vanished lists from the layout and places new ones.
    fn normalize_layout(&mut self) {
        let ids: HashSet<&str> = self.st.lists.iter().map(|l| l.id.as_str()).collect();
        let mut seen: HashSet<String> = HashSet::new();
        for col in &mut self.st.layout {
            col.retain(|id| ids.contains(id.as_str()) && seen.insert(id.clone()));
        }
        self.st.layout.retain(|c| !c.is_empty());
        for l in &self.st.lists {
            if seen.contains(&l.id) {
                continue;
            }
            if self.st.layout.len() < AUTO_COLUMNS {
                self.st.layout.push(vec![l.id.clone()]);
            } else if let Some(col) = self.st.layout.iter_mut().min_by_key(|c| c.len()) {
                col.push(l.id.clone());
            }
        }
        self.dirty = true;
    }

    fn clamp(&mut self) {
        if self.list(&self.sel.list).is_none() {
            let first = self.st.layout.first().and_then(|c| c.first()).cloned().unwrap_or_default();
            self.sel = Sel { list: first, row: 0 };
        }
        self.sel.row = self.sel.row.min(self.tile_rows(&self.sel.list).len().saturating_sub(1));
        self.follow_sel = true;
    }

    fn select(&mut self, list: &str, row: usize) {
        self.sel = Sel { list: list.to_string(), row };
        self.clamp();
    }

    fn select_task(&mut self, list: &str, id: &str) -> bool {
        let tasks = self.list_tasks(list);
        let row = self
            .tile_rows(list)
            .iter()
            .position(|r| matches!(r, TileRow::Task { task, .. } if tasks[*task].id == id));
        match row {
            Some(r) => {
                self.select(list, r);
                true
            }
            None => false,
        }
    }

    /// Up/down through the rows of a tile and on into the neighbouring tiles of the column.
    fn move_v(&mut self, delta: isize) {
        for _ in 0..delta.unsigned_abs() {
            let n = self.tile_rows(&self.sel.list).len();
            let Some((c, i)) = self.tile_pos(&self.sel.list) else { return };
            let col = &self.st.layout[c];
            if delta > 0 {
                if self.sel.row + 1 < n {
                    self.sel.row += 1;
                } else if let Some(next) = col.get(i + 1).cloned() {
                    self.sel = Sel { list: next, row: 0 };
                }
            } else if self.sel.row > 0 {
                self.sel.row -= 1;
            } else if i > 0 {
                let prev = col[i - 1].clone();
                let row = self.tile_rows(&prev).len() - 1;
                self.sel = Sel { list: prev, row };
            }
        }
        self.follow_sel = true;
    }

    /// Left/right to the tile at the same height in the next column.
    fn move_h(&mut self, dir: isize) {
        let Some((c, i)) = self.tile_pos(&self.sel.list) else { return };
        let nc = c as isize + dir;
        if nc < 0 || nc as usize >= self.st.layout.len() {
            return;
        }
        let col = &self.st.layout[nc as usize];
        let id = col[i.min(col.len() - 1)].clone();
        self.select(&id, 0);
    }

    fn jump_list(&mut self, forward: bool) {
        let all: Vec<String> = self.st.layout.concat();
        if all.is_empty() {
            return;
        }
        let i = all.iter().position(|id| *id == self.sel.list).unwrap_or(0);
        let n = all.len();
        let next = if forward { (i + 1) % n } else { (i + n - 1) % n };
        self.select(&all[next].clone(), 0);
    }

    /// Keyboard tile moving: up/down inside the column, left/right between columns
    /// (past the edge starts a new column).
    fn move_tile(&mut self, dx: isize, dy: isize) {
        let Some((c, i)) = self.tile_pos(&self.sel.list) else { return };
        let layout = &mut self.st.layout;
        if dy != 0 {
            let j = i as isize + dy;
            if j < 0 || j as usize >= layout[c].len() {
                return;
            }
            layout[c].swap(i, j as usize);
        } else {
            if layout[c].len() == 1 && ((dx < 0 && c == 0) || (dx > 0 && c + 1 == layout.len())) {
                return;
            }
            let id = layout[c].remove(i);
            let nc = c as isize + dx;
            if nc < 0 {
                layout.insert(0, vec![id]);
            } else if nc as usize >= layout.len() {
                layout.push(vec![id]);
            } else {
                let col = &mut layout[nc as usize];
                col.insert(i.min(col.len()), id);
            }
            layout.retain(|c| !c.is_empty());
        }
        self.dirty = true;
        self.follow_sel = true;
    }

    /// Where a tile dragged to `pos` would land, based on the last frame's geometry.
    pub fn drop_target(&self, pos: Position) -> Option<Drop> {
        let dragged = &self.drag.as_ref()?.list;
        if let Some((id, r)) = self.tile_rects.iter().find(|(_, r)| r.contains(pos)) {
            if id == dragged {
                return None;
            }
            let edge = (r.width / 4).max(3);
            let (dx, dy) = (pos.x - r.x, pos.y - r.y);
            return Some(if dx < edge {
                Drop::LeftOf(id.clone())
            } else if dx >= r.width.saturating_sub(edge) {
                Drop::RightOf(id.clone())
            } else if dy < r.height.div_ceil(2) {
                Drop::Above(id.clone())
            } else {
                Drop::Below(id.clone())
            });
        }
        self.col_rects.iter().find(|(_, r)| r.contains(pos)).map(|(c, _)| Drop::ColumnEnd(*c))
    }

    fn apply_drop(&mut self, list: &str, drop: Drop) {
        let layout = &mut self.st.layout;
        // Columns emptied by the removal stay until the end so indices remain valid.
        for col in layout.iter_mut() {
            col.retain(|id| id != list);
        }
        let find = |layout: &[Vec<String>], t: &str| {
            layout.iter().enumerate().find_map(|(c, col)| col.iter().position(|id| id == t).map(|i| (c, i)))
        };
        let id = list.to_string();
        match drop {
            Drop::Above(t) | Drop::Below(t) if find(layout, &t).is_none() => layout[0].push(id),
            Drop::Above(t) => {
                let (c, i) = find(layout, &t).unwrap();
                layout[c].insert(i, id);
            }
            Drop::Below(t) => {
                let (c, i) = find(layout, &t).unwrap();
                layout[c].insert(i + 1, id);
            }
            Drop::LeftOf(t) => {
                let c = find(layout, &t).map_or(0, |(c, _)| c);
                layout.insert(c, vec![id]);
            }
            Drop::RightOf(t) => {
                let c = find(layout, &t).map_or(layout.len(), |(c, _)| c + 1);
                layout.insert(c, vec![id]);
            }
            Drop::ColumnEnd(c) => match layout.get_mut(c) {
                Some(col) => col.push(id),
                None => layout.push(vec![id]),
            },
        }
        layout.retain(|c| !c.is_empty());
        self.dirty = true;
        self.select(list, 0);
    }

    pub fn save_if_dirty(&mut self) {
        if self.dirty {
            self.dirty = false;
            if let Err(e) = self.st.save() {
                self.flash(format!("Не вдалося зберегти кеш: {e}"), true);
            }
        }
    }

    pub fn save(&mut self) {
        self.dirty = true;
        self.save_if_dirty();
    }

    // ---------- sync ----------

    /// Sends at most one request at a time: queued changes first, then refreshes.
    pub fn tick(&mut self) {
        if self.busy || self.auth_error.is_some() {
            return;
        }
        if let Some(op) = self.st.pending.first() {
            if self.retry_at.is_none_or(|t| Instant::now() >= t) {
                self.busy = self.tx.send(Req::Exec(op.clone())).is_ok();
            }
            return;
        }
        let stale = self.last_refresh.is_none_or(|t| t.elapsed() >= AUTO_REFRESH);
        if self.want_refresh || stale {
            self.want_refresh = false;
            self.last_refresh = Some(Instant::now());
            self.busy = self.tx.send(Req::Refresh).is_ok();
        }
    }

    pub fn on_focus(&mut self) {
        if self.last_refresh.is_none_or(|t| t.elapsed() >= FOCUS_REFRESH) {
            self.want_refresh = true;
        }
    }

    pub fn reauth(&mut self) {
        let _ = self.tx.send(Req::Reload);
        self.auth_error = None;
        self.want_refresh = true;
    }

    fn refresh_now(&mut self) {
        if self.auth_error.is_some() {
            self.login_requested = true;
            return;
        }
        self.want_refresh = true;
        self.retry_at = None;
        self.flash("Оновлення…", false);
    }

    fn on_error(&mut self, e: ApiError) {
        match e {
            ApiError::Auth(m) => self.auth_error = Some(m),
            e => {
                self.net_error = Some(e.to_string());
                self.retry_at = Some(Instant::now() + RETRY);
            }
        }
    }

    pub fn on_resp(&mut self, r: Resp) {
        self.busy = false;
        match r {
            Resp::Done(created) => {
                let op = self.st.pending.remove(0);
                self.net_error = None;
                self.retry_at = None;
                self.dirty = true;
                match (op, created) {
                    (Op::CreateTask { list, tmp, .. }, Some(Created::Task(t))) => self.remap_task(&list, &tmp, t),
                    (Op::CreateList { tmp, .. }, Some(Created::List(l))) => self.remap_list(&tmp, &l.id),
                    _ => {}
                }
            }
            Resp::Failed(e) if e.is_transient() || matches!(e, ApiError::Auth(_)) => self.on_error(e),
            Resp::Failed(e) => {
                // Google refused this change for good; drop it so the queue keeps moving.
                self.st.pending.remove(0);
                self.dirty = true;
                self.want_refresh = true;
                self.flash(format!("Зміну не прийнято: {e}"), true);
            }
            Resp::Snapshot(lists, tasks) => {
                self.net_error = None;
                self.last_sync = Some(chrono::Local::now());
                // Local changes queued meanwhile would be overwritten; take the next snapshot.
                if self.st.pending.is_empty() {
                    let sel_task = self.selected_ids();
                    let follow = self.follow_sel;
                    self.st.lists = lists;
                    self.st.tasks = tasks;
                    let ids: HashSet<&String> = self.st.lists.iter().map(|l| &l.id).collect();
                    self.st.collapsed.retain(|id| ids.contains(id));
                    self.normalize_layout();
                    if !sel_task.is_some_and(|(l, t)| self.select_task(&l, &t)) {
                        self.clamp();
                    }
                    // A background refresh must not yank the view around.
                    self.follow_sel = follow;
                } else {
                    self.want_refresh = true;
                }
            }
            Resp::RefreshFailed(e) => self.on_error(e),
        }
    }

    fn remap_task(&mut self, list: &str, tmp: &str, real: Task) {
        if let Some(tasks) = self.st.tasks.get_mut(list) {
            for t in tasks.iter_mut() {
                if t.id == tmp {
                    t.id = real.id.clone();
                    t.position = real.position.clone();
                }
                if t.parent.as_deref() == Some(tmp) {
                    t.parent = Some(real.id.clone());
                }
            }
        }
        self.st.pending.iter_mut().for_each(|op| op.remap(tmp, &real.id));
    }

    fn remap_list(&mut self, tmp: &str, real: &str) {
        let fix = |s: &mut String| {
            if s == tmp {
                *s = real.to_string();
            }
        };
        self.st.lists.iter_mut().for_each(|l| fix(&mut l.id));
        self.st.layout.iter_mut().flatten().for_each(fix);
        fix(&mut self.sel.list);
        if let Some(v) = self.st.tasks.remove(tmp) {
            self.st.tasks.insert(real.to_string(), v);
        }
        if self.st.collapsed.remove(tmp) {
            self.st.collapsed.insert(real.to_string());
        }
        self.st.pending.iter_mut().for_each(|op| op.remap(tmp, real));
    }

    // ---------- actions (shared by keys and mouse) ----------

    fn open_input(&mut self, kind: InputKind, text: &str) {
        self.mode = Mode::Input { kind, ed: Editor::new(text) };
    }

    fn set_collapsed(&mut self, list: &str, collapsed: bool) {
        let changed = if collapsed {
            self.st.collapsed.insert(list.to_string())
        } else {
            self.st.collapsed.remove(list)
        };
        if changed {
            self.clamp();
            self.dirty = true;
        }
    }

    fn toggle_collapse(&mut self, list: &str) {
        self.set_collapsed(list, !self.is_collapsed(list));
    }

    fn expand_all(&mut self, expand: bool) {
        self.st.collapsed = if expand { HashSet::new() } else { self.st.lists.iter().map(|l| l.id.clone()).collect() };
        self.clamp();
        self.dirty = true;
    }

    fn add_task(&mut self, list: String, parent: Option<String>) {
        self.set_collapsed(&list, false);
        self.open_input(InputKind::NewTask { list, parent }, "");
    }

    /// Google Tasks allows one nesting level: a subtask of a subtask goes under its parent.
    fn add_subtask(&mut self, list: String, task: &str) {
        let parent = self
            .list_tasks(&list)
            .iter()
            .find(|t| t.id == task)
            .map(|t| t.parent.clone().unwrap_or(t.id.clone()));
        if parent.is_some() {
            self.add_task(list, parent);
        }
    }

    fn edit(&mut self, what: fn(String, String) -> InputKind, list: String, task: String) {
        let Some(t) = self.list_tasks(&list).iter().find(|t| t.id == task).cloned() else { return };
        let kind = what(list, task);
        let text = match kind {
            InputKind::Title { .. } => t.title,
            InputKind::Notes { .. } => t.notes.unwrap_or_default(),
            InputKind::Due { .. } => t.due_date().map(|d| d.format("%d.%m.%Y").to_string()).unwrap_or_default(),
            _ => String::new(),
        };
        self.open_input(kind, &text);
    }

    fn ask_delete_task(&mut self, list: String, task: String) {
        let Some(t) = self.list_tasks(&list).iter().find(|t| t.id == task) else { return };
        self.mode = Mode::Confirm { msg: format!("Видалити «{}»?", t.title), what: Confirm::DeleteTask { list, task } };
    }

    fn ask_delete_list(&mut self, list: String) {
        let Some(l) = self.list(&list) else { return };
        let n = self.list_tasks(&list).len();
        self.mode = Mode::Confirm {
            msg: format!("Видалити список «{}» і всі його завдання ({n})?", l.title),
            what: Confirm::DeleteList { list },
        };
    }

    fn ask_delete_completed(&mut self, list: String) {
        let n = self.done_count(&list);
        let Some(l) = self.list(&list) else { return };
        if n == 0 {
            self.flash("Виконаних завдань немає", false);
            return;
        }
        self.mode = Mode::Confirm {
            msg: format!("Видалити виконані завдання ({n}) зі списку «{}»?", l.title),
            what: Confirm::DeleteCompleted { list },
        };
    }

    fn rename_list(&mut self, list: String) {
        if let Some(title) = self.list(&list).map(|l| l.title.clone()) {
            self.open_input(InputKind::RenameList { list }, &title);
        }
    }

    fn toggle_done(&mut self, list: String, id: String) {
        let Some(t) = self.task_mut(&list, &id) else { return };
        let patch = if t.done() {
            t.status = "needsAction".into();
            t.completed = None;
            json!({ "status": "needsAction", "completed": null })
        } else {
            t.status = "completed".into();
            t.completed = Some(chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string());
            json!({ "status": "completed" })
        };
        self.push(Op::PatchTask { list, task: id, patch });
        self.clamp();
    }

    fn toggle_completed(&mut self) {
        let sel = self.selected_ids();
        self.st.show_completed = !self.st.show_completed;
        self.dirty = true;
        if !sel.is_some_and(|(l, t)| self.select_task(&l, &t)) {
            self.clamp();
        }
    }

    fn delete_task(&mut self, list: &str, id: &str) {
        if let Some(tasks) = self.st.tasks.get_mut(list) {
            // Google removes subtasks together with their parent.
            tasks.retain(|t| t.id != id && t.parent.as_deref() != Some(id));
        }
        self.push(Op::DeleteTask { list: list.into(), task: id.into() });
        self.clamp();
    }

    fn patch(&mut self, list: String, task: String, patch: Value) {
        self.push(Op::PatchTask { list, task, patch });
    }

    fn submit(&mut self, kind: InputKind, text: String) {
        let trimmed = text.trim().to_string();
        match kind {
            InputKind::NewTask { list, parent } => {
                if trimmed.is_empty() {
                    return;
                }
                let tmp = self.tmp_id();
                self.st.tasks.entry(list.clone()).or_default().push(Task {
                    id: tmp.clone(),
                    title: trimmed.clone(),
                    status: "needsAction".into(),
                    parent: parent.clone(),
                    ..Default::default()
                });
                self.push(Op::CreateTask { list: list.clone(), tmp: tmp.clone(), parent, title: trimmed });
                self.select_task(&list, &tmp);
            }
            InputKind::Title { list, task } => {
                if trimmed.is_empty() {
                    return;
                }
                if let Some(t) = self.task_mut(&list, &task) {
                    t.title = trimmed.clone();
                }
                self.patch(list, task, json!({ "title": trimmed }));
            }
            InputKind::Notes { list, task } => {
                let notes = text.trim_end().to_string();
                let notes = (!notes.trim().is_empty()).then_some(notes);
                if let Some(t) = self.task_mut(&list, &task) {
                    t.notes = notes.clone();
                }
                self.patch(list, task, json!({ "notes": notes }));
            }
            InputKind::Due { list, task } => match date::parse_input(&text, date::today()) {
                Ok(d) => {
                    let due = d.map(date::to_api);
                    if let Some(t) = self.task_mut(&list, &task) {
                        t.due = due.clone();
                    }
                    self.patch(list, task, json!({ "due": due }));
                }
                Err(e) => self.flash(e, true),
            },
            InputKind::NewList => {
                if trimmed.is_empty() {
                    return;
                }
                let tmp = self.tmp_id();
                self.st.lists.push(TaskList { id: tmp.clone(), title: trimmed.clone() });
                self.st.tasks.insert(tmp.clone(), vec![]);
                self.push(Op::CreateList { tmp: tmp.clone(), title: trimmed });
                self.normalize_layout();
                self.select(&tmp, 0);
            }
            InputKind::RenameList { list } => {
                if trimmed.is_empty() {
                    return;
                }
                if let Some(l) = self.st.lists.iter_mut().find(|l| l.id == list) {
                    l.title = trimmed.clone();
                }
                self.push(Op::RenameList { list, title: trimmed });
            }
        }
    }

    fn confirm(&mut self, what: Confirm) {
        match what {
            Confirm::DeleteTask { list, task } => self.delete_task(&list, &task),
            Confirm::DeleteList { list } => {
                self.st.lists.retain(|l| l.id != list);
                self.st.tasks.remove(&list);
                self.st.collapsed.remove(&list);
                self.push(Op::DeleteList { list });
                self.normalize_layout();
                self.clamp();
            }
            Confirm::DeleteCompleted { list } => {
                let ids: Vec<String> = self.list_tasks(&list).iter().filter(|t| t.done()).map(|t| t.id.clone()).collect();
                for id in ids {
                    self.delete_task(&list, &id);
                }
            }
        }
    }

    fn hide(&mut self) {
        if self.pocket {
            let _ = std::process::Command::new("hyprctl")
                .args(["dispatch", r#"hl.dsp.workspace.toggle_special("ptask")"#])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        } else {
            self.quit = true;
        }
    }

    // ---------- mouse ----------

    pub fn on_mouse(&mut self, m: MouseEvent) {
        let pos = Position::new(m.column, m.row);
        self.hover = Some(pos);
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let from = if matches!(self.mode, Mode::Normal) { 0 } else { self.modal_from.min(self.hits.len()) };
                let hit = self.hits[from..].iter().rev().find(|(r, _)| r.contains(pos)).map(|(_, c)| c.clone());
                match hit {
                    Some(Click::DragTile(list)) => self.drag = Some(Drag { list, from: pos, pos, moved: false }),
                    Some(c) => self.on_click(c),
                    // Clicking outside a dialog closes it, except while typing.
                    None if matches!(self.mode, Mode::Confirm { .. } | Mode::Help) => self.mode = Mode::Normal,
                    // Clicking the dimmed backdrop puts the tracker back in the pocket.
                    None if matches!(self.mode, Mode::Normal) && self.pocket && !self.panel.contains(pos) => self.hide(),
                    None => {}
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(d) = &mut self.drag {
                    d.pos = pos;
                    d.moved |= pos != d.from;
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(d) = self.drag.take() {
                    if !d.moved {
                        self.on_click(Click::Select(d.list, 0));
                    } else {
                        let list = d.list.clone();
                        self.drag = Some(Drag { pos, ..d });
                        let target = self.drop_target(pos);
                        self.drag = None;
                        if let Some(t) = target {
                            self.apply_drop(&list, t);
                        }
                    }
                }
            }
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp if matches!(self.mode, Mode::Normal) => {
                let delta: isize = if m.kind == MouseEventKind::ScrollDown { 1 } else { -1 };
                self.scroll_at(pos, delta);
            }
            _ => {}
        }
    }

    /// Wheel: scroll the tile under the cursor if it has hidden rows, else its column.
    fn scroll_at(&mut self, pos: Position, delta: isize) {
        if let Some((id, r)) = self.tile_rects.iter().find(|(_, r)| r.contains(pos)).cloned() {
            let hidden = self.tile_rows(&id).len().saturating_sub(1) > r.height.saturating_sub(2) as usize;
            if hidden {
                let s = self.tile_scroll.entry(id).or_default();
                *s = s.saturating_add_signed(delta * 2);
                return;
            }
        }
        if let Some(&(c, _)) = self.col_rects.iter().find(|(_, r)| r.contains(pos)) {
            if let Some(s) = self.col_scroll.get_mut(c) {
                *s = s.saturating_add_signed(delta);
            }
        }
    }

    fn on_click(&mut self, c: Click) {
        let double = self.last_click.as_ref().is_some_and(|(t, prev)| t.elapsed() < DOUBLE_CLICK && *prev == c);
        self.last_click = Some((Instant::now(), c.clone()));
        if let Mode::Help = self.mode {
            self.mode = Mode::Normal;
            return;
        }
        match c {
            Click::Select(list, row) => {
                self.select(&list, row);
                match self.sel_row() {
                    Some(TileRow::Header) if double => self.toggle_collapse(&list),
                    Some(TileRow::Task { .. }) if double => {
                        if let Some((l, t)) = self.selected_ids() {
                            self.edit(|list, task| InputKind::Title { list, task }, l, t);
                        }
                    }
                    Some(TileRow::Add) => self.add_task(list, None),
                    _ => {}
                }
            }
            Click::DragTile(list) => self.select(&list, 0),
            Click::ToggleCollapse(l) => self.toggle_collapse(&l),
            Click::ToggleDone(l, t) => self.toggle_done(l, t),
            Click::AddTask(l) => self.add_task(l, None),
            Click::AddSubtask(l, t) => self.add_subtask(l, &t),
            Click::EditTitle(l, t) => self.edit(|list, task| InputKind::Title { list, task }, l, t),
            Click::EditNotes(l, t) => self.edit(|list, task| InputKind::Notes { list, task }, l, t),
            Click::EditDue(l, t) => self.edit(|list, task| InputKind::Due { list, task }, l, t),
            Click::DeleteTask(l, t) => self.ask_delete_task(l, t),
            Click::DeleteCompleted(l) => self.ask_delete_completed(l),
            Click::AddList => self.open_input(InputKind::NewList, ""),
            Click::RenameList(l) => self.rename_list(l),
            Click::DeleteList(l) => self.ask_delete_list(l),
            Click::ExpandAll(e) => self.expand_all(e),
            Click::ColScroll(c, d) => {
                if c == usize::MAX {
                    self.col_offset = self.col_offset.saturating_add_signed(d as isize);
                } else if let Some(s) = self.col_scroll.get_mut(c) {
                    *s = s.saturating_add_signed(d as isize);
                }
            }
            Click::ToggleCompleted => self.toggle_completed(),
            Click::Refresh => self.refresh_now(),
            Click::Login => self.login_requested = true,
            Click::Help => self.mode = Mode::Help,
            Click::Hide => self.hide(),
            Click::Cancel | Click::No => self.mode = Mode::Normal,
            Click::Save => {
                if let Mode::Input { kind, ed } = std::mem::replace(&mut self.mode, Mode::Normal) {
                    self.submit(kind, ed.text());
                }
            }
            Click::Yes => {
                if let Mode::Confirm { what, .. } = std::mem::replace(&mut self.mode, Mode::Normal) {
                    self.confirm(what);
                }
            }
            Click::DuePreset(p) => {
                if let Mode::Input { kind: kind @ InputKind::Due { .. }, .. } = std::mem::replace(&mut self.mode, Mode::Normal) {
                    self.submit(kind, p.to_string());
                }
            }
        }
    }

    // ---------- keys ----------

    pub fn on_key(&mut self, k: KeyEvent) {
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        self.follow_sel = true;
        match std::mem::replace(&mut self.mode, Mode::Normal) {
            Mode::Normal => self.key_normal(k),
            Mode::Help => {}
            Mode::Confirm { what, msg } => match k.code {
                KeyCode::Enter => self.confirm(what),
                KeyCode::Char(c) if latin(c) == 'y' => self.confirm(what),
                KeyCode::Char(c) if latin(c) == 'n' => {}
                KeyCode::Esc => {}
                _ => self.mode = Mode::Confirm { what, msg },
            },
            Mode::Input { kind, mut ed } => {
                let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
                let alt = k.modifiers.contains(KeyModifiers::ALT);
                match k.code {
                    KeyCode::Esc => {}
                    KeyCode::Char('s') if ctrl => self.submit(kind, ed.text()),
                    KeyCode::Enter if kind.multiline() && !alt && !ctrl => {
                        ed.insert('\n');
                        self.mode = Mode::Input { kind, ed };
                    }
                    KeyCode::Enter => self.submit(kind, ed.text()),
                    _ => {
                        ed.key(k);
                        self.mode = Mode::Input { kind, ed };
                    }
                }
            }
        }
    }

    fn key_normal(&mut self, k: KeyEvent) {
        let code = match k.code {
            KeyCode::Char(c) => KeyCode::Char(latin(c)),
            c => c,
        };
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match code {
            KeyCode::Char('l') if ctrl => return self.login_requested = true,
            KeyCode::Char('Q') => return self.quit = true,
            KeyCode::Char('q') | KeyCode::Esc => return self.hide(),
            KeyCode::Char('?') | KeyCode::F(1) => return self.mode = Mode::Help,
            KeyCode::Char('r') => return self.refresh_now(),
            KeyCode::Char('c') => return self.toggle_completed(),
            KeyCode::Char('N') => return self.open_input(InputKind::NewList, ""),
            KeyCode::Char('z') => return self.expand_all(false),
            KeyCode::Char('Z') => return self.expand_all(true),
            KeyCode::Char('H') => return self.move_tile(-1, 0),
            KeyCode::Char('L') => return self.move_tile(1, 0),
            KeyCode::Char('K') => return self.move_tile(0, -1),
            KeyCode::Char('J') => return self.move_tile(0, 1),
            KeyCode::Left if shift => return self.move_tile(-1, 0),
            KeyCode::Right if shift => return self.move_tile(1, 0),
            KeyCode::Up if shift => return self.move_tile(0, -1),
            KeyCode::Down if shift => return self.move_tile(0, 1),
            KeyCode::Char('j') | KeyCode::Down => return self.move_v(1),
            KeyCode::Char('k') | KeyCode::Up => return self.move_v(-1),
            KeyCode::Char('h') | KeyCode::Left => return self.move_h(-1),
            KeyCode::Char('l') | KeyCode::Right => return self.move_h(1),
            KeyCode::PageDown => return self.move_v(10),
            KeyCode::PageUp => return self.move_v(-10),
            KeyCode::Char('g') | KeyCode::Home => return self.sel.row = 0,
            KeyCode::Char('G') | KeyCode::End => return self.sel.row = self.tile_rows(&self.sel.list).len() - 1,
            KeyCode::Tab | KeyCode::Char(']') => return self.jump_list(true),
            KeyCode::BackTab | KeyCode::Char('[') => return self.jump_list(false),
            _ => {}
        }
        let list = self.sel.list.clone();
        if self.list(&list).is_none() {
            return;
        }
        match self.sel_row() {
            Some(TileRow::Header) => match code {
                KeyCode::Enter | KeyCode::Char(' ') => self.toggle_collapse(&list),
                KeyCode::Char('a') | KeyCode::Char('o') | KeyCode::Char('n') => self.add_task(list, None),
                KeyCode::Char('e') | KeyCode::Char('R') => self.rename_list(list),
                KeyCode::Char('x') | KeyCode::Char('D') | KeyCode::Delete => self.ask_delete_list(list),
                KeyCode::Char('C') => self.ask_delete_completed(list),
                _ => {}
            },
            Some(TileRow::Add) => match code {
                KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Char('a') | KeyCode::Char('o') | KeyCode::Char('n') => {
                    self.add_task(list, None)
                }
                _ => {}
            },
            Some(TileRow::Task { .. }) => {
                let Some((_, task)) = self.selected_ids() else { return };
                match code {
                    KeyCode::Char(' ') => self.toggle_done(list, task),
                    KeyCode::Enter | KeyCode::Char('e') => self.edit(|list, task| InputKind::Title { list, task }, list, task),
                    KeyCode::Char('i') => self.edit(|list, task| InputKind::Notes { list, task }, list, task),
                    KeyCode::Char('d') => self.edit(|list, task| InputKind::Due { list, task }, list, task),
                    KeyCode::Char('a') | KeyCode::Char('o') | KeyCode::Char('n') => self.add_task(list, None),
                    KeyCode::Char('A') | KeyCode::Char('O') => self.add_subtask(list, &task),
                    KeyCode::Char('x') | KeyCode::Char('D') | KeyCode::Delete => self.ask_delete_task(list, task),
                    KeyCode::Char('C') => self.ask_delete_completed(list),
                    _ => {}
                }
            }
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_moves_between_lines() {
        let mut e = Editor::new("abc\nde");
        e.up();
        assert_eq!(e.cur, 2);
        e.down();
        assert_eq!(e.cur, 6);
        e.delete_word_back();
        assert_eq!(e.text(), "abc\n");
    }

    #[test]
    fn maps_ukrainian_layout() {
        assert_eq!(latin('о'), 'j');
        assert_eq!(latin('л'), 'k');
        assert_eq!(latin('Ч'), 'X');
        assert_eq!(latin('х'), '[');
        assert_eq!(latin('j'), 'j');
    }
}
