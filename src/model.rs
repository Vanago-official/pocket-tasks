use crate::config::{state_path, write_atomic};
use crate::date;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TaskList {
    pub id: String,
    #[serde(default)]
    pub title: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<String>,
    /// "needsAction" | "completed"
    #[serde(default)]
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default)]
    pub position: String,
    #[serde(default, skip_serializing)]
    pub deleted: bool,
}

impl Task {
    pub fn done(&self) -> bool {
        self.status == "completed"
    }
    pub fn due_date(&self) -> Option<NaiveDate> {
        self.due.as_deref().and_then(date::parse_api)
    }
}

/// A change made locally that still has to reach Google.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum Op {
    CreateList { tmp: String, title: String },
    RenameList { list: String, title: String },
    DeleteList { list: String },
    CreateTask { list: String, tmp: String, parent: Option<String>, title: String },
    PatchTask { list: String, task: String, patch: Value },
    DeleteTask { list: String, task: String },
    /// Move a task (with its subtasks) to another list and/or under another task.
    MoveTask {
        list: String,
        task: String,
        dest: String,
        #[serde(default)]
        parent: Option<String>,
    },
}

impl Op {
    /// Swap a temporary id for the real one once Google has created the object.
    pub fn remap(&mut self, from: &str, to: &str) {
        let fix = |s: &mut String| {
            if s == from {
                *s = to.to_string();
            }
        };
        match self {
            Op::CreateList { tmp, .. } => fix(tmp),
            Op::RenameList { list, .. } | Op::DeleteList { list } => fix(list),
            Op::CreateTask { list, tmp, parent, .. } => {
                fix(list);
                fix(tmp);
                if let Some(p) = parent {
                    fix(p);
                }
            }
            Op::PatchTask { list, task, .. } | Op::DeleteTask { list, task } => {
                fix(list);
                fix(task);
            }
            Op::MoveTask { list, task, dest, parent } => {
                fix(list);
                fix(task);
                fix(dest);
                if let Some(p) = parent {
                    fix(p);
                }
            }
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
pub struct State {
    pub lists: Vec<TaskList>,
    pub tasks: HashMap<String, Vec<Task>>,
    pub pending: Vec<Op>,
    #[serde(default)]
    pub show_completed: bool,
    /// Lists folded in the tree view; kept across restarts.
    #[serde(default)]
    pub collapsed: HashSet<String>,
    /// Tasks whose subtasks are folded away; kept across restarts.
    #[serde(default)]
    pub collapsed_tasks: HashSet<String>,
    /// Board layout: columns of list ids, top to bottom.
    #[serde(default)]
    pub layout: Vec<Vec<String>>,
}

impl State {
    pub fn load() -> Self {
        std::fs::read_to_string(state_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        write_atomic(&state_path(), &serde_json::to_vec(self).unwrap(), false)
    }
}
