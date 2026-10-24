use crate::api::ApiError;
use chrono::NaiveDate;
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

/// What is being dragged: a whole tile, or one task on its way to another list.
#[derive(Clone, PartialEq, Debug)]
pub enum DragItem {
    Tile(String),
    Task { list: String, task: String },
}

/// Destination of a dragged task: a list, and the task it becomes a subtask of.
#[derive(Clone, PartialEq, Debug)]
pub struct TaskDrop {
    pub list: String,
    pub parent: Option<String>,
}

pub struct Drag {
    pub item: DragItem,
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
    /// Fold or unfold the subtasks of a task.
    ToggleSubtasks(String),
    ToggleDone(String, String),
    AddTask(String),
    AddSubtask(String, String),
    EditTitle(String, String),
    EditNotes(String, String),
    EditDue(String, String),
    DeleteTask(String, String),
    /// Open the "move to list" picker for a task.
    MoveTask(String, String),
    /// Pick a destination list in that picker.
    MoveTo(String),
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
    /// Rename several tasks through a template; `*` stands for the part that differs
    /// (`pre`/`suf` chars of each old title are the shared prefix/suffix).
    Rename { tasks: Vec<(String, String)>, pre: usize, suf: usize },
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
    /// Choosing the destination list for a task; `sel` indexes `move_targets()`.
    MoveTo { list: String, task: String, sel: usize },
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
/// Lengths of the prefix and suffix all titles share, never cutting a word or number
/// in two, so `Practice 7` / `Practice 17` share `Practice `, not `Practice `+`7`.
fn common_affixes(titles: &[Vec<char>]) -> (usize, usize) {
    let Some(first) = titles.first() else { return (0, 0) };
    let word = |c: Option<&char>| c.is_some_and(|c| c.is_alphanumeric());
    let mut pre = titles.iter().map(|t| t.iter().zip(first).take_while(|(a, b)| a == b).count()).min().unwrap_or(0);
    while pre > 0 && word(first.get(pre - 1)) && titles.iter().any(|t| word(t.get(pre))) {
        pre -= 1;
    }
    let room = titles.iter().map(|t| t.len() - pre).min().unwrap_or(0);
    let mut suf = titles
        .iter()
        .map(|t| t.iter().rev().zip(first.iter().rev()).take_while(|(a, b)| a == b).count())
        .min()
        .unwrap_or(0)
        .min(room);
    while suf > 0
        && word(first.get(first.len() - suf))
        && titles.iter().any(|t| t.len() > suf && t.len() - suf > pre && word(t.get(t.len() - suf - 1)))
    {
        suf -= 1;
    }
    (pre, suf)
}

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
    /// Where a dragged task would land, worked out from the previous frame.
    pub task_drop: Option<TaskDrop>,
    /// Tasks picked with Ctrl+click / `v`, in picking order.
    pub marked: Vec<String>,
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
            task_drop: None,
            marked: Vec::new(),
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

    /// Subtasks of a task that the tile would show when unfolded.
    pub fn subtask_count(&self, list: &str, task: &str) -> usize {
        let show_done = self.st.show_completed;
        self.list_tasks(list).iter().filter(|t| t.parent.as_deref() == Some(task) && (show_done || !t.done())).count()
    }

    /// Nearest deadline among the open subtasks of a task.
    pub fn nearest_subtask_due(&self, list: &str, task: &str) -> Option<NaiveDate> {
        self.list_tasks(list)
            .iter()
            .filter(|t| t.parent.as_deref() == Some(task) && !t.done())
            .filter_map(|t| t.due_date())
            .min()
    }

    /// Tasks of one list as (index into its vec, depth): open tasks with their subtasks,
    /// nearest deadline first (undated ones after, in Google's order), then completed
    /// ones (newest first).
    pub fn task_rows(&self, list: &str) -> Vec<(usize, u8)> {
        let tasks = self.list_tasks(list);
        let ids: HashSet<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
        let show_done = self.st.show_completed;
        let by_due = |&a: &usize, &b: &usize| {
            let key = |i: usize| (tasks[i].due_date().is_none(), tasks[i].due_date(), &tasks[i].position);
            key(a).cmp(&key(b))
        };
        let children = |pid: &str| {
            let mut c: Vec<usize> = (0..tasks.len())
                .filter(|&i| tasks[i].parent.as_deref() == Some(pid) && (show_done || !tasks[i].done()))
                .collect();
            c.sort_by(|a, b| tasks[*a].done().cmp(&tasks[*b].done()).then_with(|| by_due(a, b)));
            c
        };
        let top: Vec<usize> = (0..tasks.len())
            .filter(|&i| tasks[i].parent.as_deref().is_none_or(|p| !ids.contains(p)))
            .collect();
        let mut open: Vec<usize> = top.iter().copied().filter(|&i| !tasks[i].done()).collect();
        open.sort_by(by_due);
        let mut done: Vec<usize> =
            if show_done { top.iter().copied().filter(|&i| tasks[i].done()).collect() } else { vec![] };
        done.sort_by(|&a, &b| tasks[b].completed.cmp(&tasks[a].completed));

        let mut out = Vec::new();
        for i in open.into_iter().chain(done) {
            out.push((i, 0));
            if !self.st.collapsed_tasks.contains(&tasks[i].id) {
                out.extend(children(&tasks[i].id).into_iter().map(|c| (c, 1)));
            }
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

    #[cfg(test)]
    pub fn select_task_pub(&mut self, list: &str, id: &str) -> bool {
        self.select_task(list, id)
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
        let Some(DragItem::Tile(dragged)) = self.drag.as_ref().map(|d| &d.item) else { return None };
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

    /// Where a dragged task would land: onto a task it becomes that task's subtask
    /// (Google allows one level, so onto a subtask means its parent), onto the rest of
    /// a tile it becomes a top-level task of that list.
    pub fn task_drop_target(&self, pos: Position) -> Option<TaskDrop> {
        let Some(DragItem::Task { list, task }) = self.drag.as_ref().map(|d| &d.item) else { return None };
        let row_hit = self.hits.iter().rev().find_map(|(r, c)| match c {
            Click::Select(l, row) if r.contains(pos) => Some((l.clone(), *row)),
            _ => None,
        });
        let target = match row_hit {
            Some((l, row)) => {
                let parent = match self.tile_rows(&l).get(row) {
                    Some(TileRow::Task { task: ti, .. }) => {
                        let t = &self.list_tasks(&l)[*ti];
                        Some(t.parent.clone().unwrap_or_else(|| t.id.clone()))
                    }
                    _ => None,
                };
                TaskDrop { list: l, parent }
            }
            None => {
                let (l, _) = self.tile_rects.iter().find(|(_, r)| r.contains(pos))?;
                TaskDrop { list: l.clone(), parent: None }
            }
        };
        let current = self.list_tasks(list).iter().find(|t| t.id == *task)?.parent.clone();
        let unchanged = target.list == *list && target.parent == current;
        (!unchanged && target.parent.as_deref() != Some(task)).then_some(target)
    }

    /// Why a task cannot be nested, if it cannot.
    pub fn nest_blocker(&self, list: &str, task: &str, parent: Option<&str>) -> Option<&'static str> {
        let has_children = self.list_tasks(list).iter().any(|t| t.parent.as_deref() == Some(task));
        (parent.is_some() && has_children).then_some("завдання з підзавданнями не можна вкласти")
    }

    /// Lists a task can move to, in board order.
    pub fn move_targets(&self, from: &str) -> Vec<String> {
        self.st.layout.concat().into_iter().filter(|id| id != from).collect()
    }

    fn open_move(&mut self, list: String, task: String) {
        if self.move_targets(&list).is_empty() {
            self.flash("Немає іншого списку", true);
            return;
        }
        self.mode = Mode::MoveTo { list, task, sel: 0 };
    }

    fn list_of(&self, task: &str) -> Option<String> {
        self.st.tasks.iter().find(|(_, ts)| ts.iter().any(|t| t.id == task)).map(|(l, _)| l.clone())
    }

    pub fn is_marked(&self, task: &str) -> bool {
        self.marked.iter().any(|m| m == task)
    }

    /// Tasks an action on `task` applies to: the whole marked group when it includes it.
    pub fn group(&self, list: &str, task: &str) -> Vec<(String, String)> {
        if !self.is_marked(task) {
            return vec![(list.to_string(), task.to_string())];
        }
        self.marked.iter().filter_map(|id| Some((self.list_of(id)?, id.clone()))).collect()
    }

    fn toggle_mark(&mut self, task: &str) {
        if self.is_marked(task) {
            self.marked.retain(|m| m != task);
        } else {
            self.marked.push(task.to_string());
        }
    }

    /// Marks every task between the selected row and `row` of the same tile.
    fn mark_range(&mut self, list: &str, row: usize) {
        let from = if self.sel.list == list { self.sel.row } else { row };
        let rows = self.tile_rows(list);
        let tasks = self.list_tasks(list);
        let ids: Vec<String> = (from.min(row)..=from.max(row))
            .filter_map(|r| match rows.get(r) {
                Some(TileRow::Task { task, .. }) => Some(tasks[*task].id.clone()),
                _ => None,
            })
            .collect();
        for id in ids {
            if !self.is_marked(&id) {
                self.marked.push(id);
            }
        }
    }

    fn clear_marks(&mut self) {
        self.marked.clear();
    }

    /// Moves `task`, or its whole marked group, like a single drop.
    fn move_group(&mut self, list: &str, task: &str, dest: &str, parent: Option<String>) {
        let group = self.group(list, task);
        if group.len() < 2 {
            return self.move_task(list, task, dest, parent);
        }
        let ids: HashSet<&str> = group.iter().map(|(_, t)| t.as_str()).collect();
        let (mut moved, mut blocked) = (0, 0);
        let mut todo = Vec::new();
        for (l, id) in &group {
            let Some(t) = self.list_tasks(l).iter().find(|t| t.id == *id) else { continue };
            // Subtasks of a marked parent travel with it; nothing nests into itself.
            if t.parent.as_deref().is_some_and(|p| ids.contains(p)) || parent.as_deref() == Some(id) {
                continue;
            }
            if l == dest && t.parent == parent {
                continue;
            }
            if self.nest_blocker(l, id, parent.as_deref()).is_some() {
                blocked += 1;
                continue;
            }
            todo.push((l.clone(), id.clone()));
        }
        for (l, id) in todo {
            self.move_task(&l, &id, dest, parent.clone());
            moved += 1;
        }
        let mut msg = format!("Перенесено завдань: {moved}");
        if blocked > 0 {
            msg += &format!(" · {blocked} з підзавданнями не вкласти");
        }
        self.flash(msg, blocked > 0);
    }

    fn move_task(&mut self, list: &str, id: &str, dest: &str, parent: Option<String>) {
        if self.list(dest).is_none() {
            return;
        }
        if let Some(why) = self.nest_blocker(list, id, parent.as_deref()) {
            self.flash(format!("Не можна: {why}"), true);
            return;
        }
        let parent_title = parent
            .as_deref()
            .and_then(|p| self.list_tasks(dest).iter().find(|t| t.id == p))
            .map(|t| t.title.clone());
        let msg = match &parent_title {
            Some(p) => format!("Тепер підзавдання «{p}»"),
            None if list == dest => "Тепер окреме завдання".to_string(),
            None => format!("Перенесено в «{}»", self.list(dest).map(|l| l.title.as_str()).unwrap_or("")),
        };
        if list == dest {
            let Some(t) = self.task_mut(list, id) else { return };
            if t.parent == parent {
                return;
            }
            t.parent = parent.clone();
            t.position = String::new();
            self.push(Op::MoveTask { list: list.into(), task: id.into(), dest: dest.into(), parent });
            self.flash(msg, false);
            self.select_task(dest, id);
            return;
        }
        let Some(src) = self.st.tasks.get_mut(list) else { return };
        // Subtasks travel with their parent; a moved subtask becomes a top-level task.
        let (mut moved, keep): (Vec<Task>, Vec<Task>) =
            src.drain(..).partition(|t| t.id == id || t.parent.as_deref() == Some(id));
        *src = keep;
        if moved.is_empty() {
            return;
        }
        for t in &mut moved {
            if t.id == id {
                t.parent = parent.clone();
                t.position = String::new();
            }
        }
        self.st.tasks.entry(dest.to_string()).or_default().extend(moved);
        self.push(Op::MoveTask { list: list.into(), task: id.into(), dest: dest.into(), parent });
        self.flash(msg, false);
        if !self.select_task(dest, id) {
            self.select(dest, 0);
        }
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
                    let task_ids: HashSet<&String> = self.st.tasks.values().flatten().map(|t| &t.id).collect();
                    self.st.collapsed_tasks.retain(|id| task_ids.contains(id));
                    self.marked.retain(|id| task_ids.contains(id));
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
        for m in self.marked.iter_mut().filter(|m| *m == tmp) {
            *m = real.id.clone();
        }
        if self.st.collapsed_tasks.remove(tmp) {
            self.st.collapsed_tasks.insert(real.id.clone());
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

    /// Folds a task's subtasks; a selected subtask hands the selection to its parent.
    fn toggle_subtasks(&mut self, list: &str, task: &str) {
        let parent = self.list_tasks(list).iter().find(|t| t.id == task).and_then(|t| t.parent.clone());
        // On a subtask the toggle applies to its parent.
        let task = parent.unwrap_or_else(|| task.to_string());
        if self.subtask_count(list, &task) == 0 {
            return;
        }
        // Checked before folding: afterwards the row indices no longer match.
        let on_child = self.sel.list == list && self.selected_task().is_some_and(|t| t.parent.as_deref() == Some(&task));
        if !self.st.collapsed_tasks.remove(&task) {
            self.st.collapsed_tasks.insert(task.clone());
            if on_child {
                self.select_task(list, &task);
            }
        }
        self.clamp();
        self.dirty = true;
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
        if let InputKind::Title { list, task } = &kind {
            let group = self.group(list, task);
            if group.len() > 1 {
                return self.rename_group(group);
            }
        }
        let text = match kind {
            InputKind::Title { .. } => t.title,
            InputKind::Notes { .. } => t.notes.unwrap_or_default(),
            InputKind::Due { .. } => t.due_date().map(|d| d.format("%d.%m.%Y").to_string()).unwrap_or_default(),
            _ => String::new(),
        };
        self.open_input(kind, &text);
    }

    fn rename_group(&mut self, tasks: Vec<(String, String)>) {
        let titles: Vec<Vec<char>> = tasks
            .iter()
            .filter_map(|(l, id)| self.list_tasks(l).iter().find(|t| t.id == *id))
            .map(|t| t.title.chars().collect())
            .collect();
        let (pre, suf) = common_affixes(&titles);
        let first = &titles[0];
        let template: String =
            first[..pre].iter().chain(['*'].iter()).chain(first[first.len() - suf..].iter()).collect();
        self.open_input(InputKind::Rename { tasks, pre, suf }, &template);
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
        let left: HashSet<String> = self.st.tasks.values().flatten().map(|t| t.id.clone()).collect();
        self.marked.retain(|m| left.contains(m));
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
            InputKind::Rename { tasks, pre, suf } => {
                let mut renamed = 0;
                for (list, task) in tasks {
                    let Some(t) = self.list_tasks(&list).iter().find(|t| t.id == task) else { continue };
                    let old: Vec<char> = t.title.chars().collect();
                    let a = pre.min(old.len());
                    let b = old.len().saturating_sub(suf).max(a);
                    let part: String = old[a..b].iter().collect();
                    let title = trimmed.replacen('*', &part, 1).trim().to_string();
                    if title.is_empty() || title == t.title {
                        continue;
                    }
                    if let Some(t) = self.task_mut(&list, &task) {
                        t.title = title.clone();
                    }
                    self.patch(list, task, json!({ "title": title }));
                    renamed += 1;
                }
                self.flash(format!("Перейменовано завдань: {renamed}"), false);
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
                    Some(Click::DragTile(list)) => {
                        self.drag = Some(Drag { item: DragItem::Tile(list), from: pos, pos, moved: false })
                    }
                    Some(Click::Select(list, row)) if matches!(self.mode, Mode::Normal) => {
                        let row_task = match self.tile_rows(&list).get(row) {
                            Some(TileRow::Task { task, .. }) => Some(self.list_tasks(&list)[*task].id.clone()),
                            _ => None,
                        };
                        if let Some(id) = &row_task {
                            // Ctrl+click picks one task, Alt+click a run of them.
                            if m.modifiers.contains(KeyModifiers::CONTROL) {
                                self.toggle_mark(id);
                                return self.select(&list, row);
                            }
                            if m.modifiers.contains(KeyModifiers::ALT) {
                                self.mark_range(&list, row);
                                return self.select(&list, row);
                            }
                            if !self.is_marked(id) {
                                self.clear_marks();
                            }
                        }
                        self.on_click(Click::Select(list.clone(), row));
                        // A task row can be dragged onto another tile.
                        if let (Mode::Normal, Some((l, task))) = (&self.mode, self.selected_ids()) {
                            if l == list {
                                self.drag = Some(Drag { item: DragItem::Task { list, task }, from: pos, pos, moved: false });
                            }
                        }
                    }
                    Some(c) => self.on_click(c),
                    // Clicking outside a dialog closes it, except while typing.
                    None if matches!(self.mode, Mode::Confirm { .. } | Mode::Help | Mode::MoveTo { .. }) => {
                        self.mode = Mode::Normal
                    }
                    // Clicking the dimmed backdrop puts the tracker back in the pocket.
                    None if matches!(self.mode, Mode::Normal) && self.pocket && !self.panel.contains(pos) => self.hide(),
                    None => {}
                }
            }
            MouseEventKind::Down(MouseButton::Right) if matches!(self.mode, Mode::Normal) => {
                let hit = self.hits.iter().rev().find(|(r, _)| r.contains(pos)).map(|(_, c)| c.clone());
                if let Some(Click::Select(list, row)) = hit {
                    if let Some(TileRow::Task { task, .. }) = self.tile_rows(&list).get(row) {
                        let id = self.list_tasks(&list)[*task].id.clone();
                        self.toggle_mark(&id);
                        self.select(&list, row);
                    }
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
                    let moved = d.moved;
                    self.drag = Some(Drag { pos, ..d });
                    let (tile_target, task_target) = (self.drop_target(pos), self.task_drop_target(pos));
                    let d = self.drag.take().unwrap();
                    match d.item {
                        DragItem::Tile(list) if !moved => self.on_click(Click::Select(list, 0)),
                        DragItem::Tile(list) => {
                            if let Some(t) = tile_target {
                                self.apply_drop(&list, t);
                            }
                        }
                        DragItem::Task { list, task } if moved && matches!(self.mode, Mode::Normal) => {
                            if let Some(TaskDrop { list: dest, parent }) = task_target {
                                self.move_group(&list, &task, &dest, parent);
                            }
                        }
                        DragItem::Task { .. } => {}
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
            Click::ToggleSubtasks(t) => {
                let list = self.st.tasks.iter().find(|(_, ts)| ts.iter().any(|x| x.id == t)).map(|(l, _)| l.clone());
                if let Some(l) = list {
                    self.toggle_subtasks(&l, &t);
                }
            }
            Click::ToggleDone(l, t) => self.toggle_done(l, t),
            Click::AddTask(l) => self.add_task(l, None),
            Click::AddSubtask(l, t) => self.add_subtask(l, &t),
            Click::EditTitle(l, t) => self.edit(|list, task| InputKind::Title { list, task }, l, t),
            Click::EditNotes(l, t) => self.edit(|list, task| InputKind::Notes { list, task }, l, t),
            Click::EditDue(l, t) => self.edit(|list, task| InputKind::Due { list, task }, l, t),
            Click::DeleteTask(l, t) => self.ask_delete_task(l, t),
            Click::MoveTask(l, t) => self.open_move(l, t),
            Click::MoveTo(dest) => {
                if let Mode::MoveTo { list, task, .. } = std::mem::replace(&mut self.mode, Mode::Normal) {
                    self.move_group(&list, &task, &dest, None);
                }
            }
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
            Mode::MoveTo { list, task, sel } => {
                let targets = self.move_targets(&list);
                let n = targets.len();
                let code = match k.code {
                    KeyCode::Char(c) => KeyCode::Char(latin(c)),
                    c => c,
                };
                match code {
                    KeyCode::Esc | KeyCode::Char('q') => {}
                    KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Char('l') => {
                        if let Some(dest) = targets.get(sel) {
                            self.move_group(&list, &task, dest, None);
                        }
                    }
                    KeyCode::Char(c @ '1'..='9') => match targets.get(c as usize - '1' as usize) {
                        Some(dest) => self.move_group(&list, &task, dest, None),
                        None => self.mode = Mode::MoveTo { list, task, sel },
                    },
                    KeyCode::Char('j') | KeyCode::Down => self.mode = Mode::MoveTo { list, task, sel: (sel + 1) % n },
                    KeyCode::Char('k') | KeyCode::Up => self.mode = Mode::MoveTo { list, task, sel: (sel + n - 1) % n },
                    _ => self.mode = Mode::MoveTo { list, task, sel },
                }
            }
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
            KeyCode::Esc if !self.marked.is_empty() => return self.clear_marks(),
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
                    KeyCode::Char('m') => self.open_move(list, task),
                    KeyCode::Char('s') => self.toggle_subtasks(&list, &task),
                    KeyCode::Char('v') => {
                        self.toggle_mark(&task);
                        self.move_v(1);
                    }
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
    fn nearest_deadline_first() {
        let mut st = State::default();
        st.lists.push(TaskList { id: "l".into(), title: "L".into() });
        let t = |id: &str, due: Option<&str>, parent: Option<&str>| Task {
            id: id.into(),
            title: id.into(),
            status: "needsAction".into(),
            due: due.map(|d| format!("{d}T00:00:00.000Z")),
            parent: parent.map(String::from),
            position: id.into(),
            ..Default::default()
        };
        st.tasks.insert(
            "l".into(),
            vec![
                t("a", None, None),
                t("b", Some("2026-11-05"), None),
                t("c", Some("2026-10-30"), None),
                t("d", None, None),
                t("e", Some("2026-11-01"), Some("b")),
                t("f", Some("2026-10-31"), Some("b")),
            ],
        );
        let app = App::new(st, std::sync::mpsc::channel().0, false);
        let order: Vec<&str> =
            app.task_rows("l").iter().map(|&(i, _)| app.list_tasks("l")[i].id.as_str()).collect();
        assert_eq!(order, ["c", "b", "f", "e", "a", "d"]);
    }

    #[test]
    fn shared_title_parts() {
        let t = |v: &[&str]| common_affixes(&v.iter().map(|s| s.chars().collect()).collect::<Vec<_>>());
        assert_eq!(t(&["Networking Concepts Practice 7", "Networking Concepts Practice 8"]), (29, 0));
        assert_eq!(t(&["Practice 17", "Practice 18"]), (9, 0));
        assert_eq!(t(&["Lab 1 notes", "Lab 2 notes"]), (4, 6));
        assert_eq!(t(&["Звіт", "Лист"]), (0, 0));
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
