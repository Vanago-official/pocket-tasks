//! Background thread that talks to Google so the UI never blocks on the network.

use crate::api::{Api, ApiError};
use crate::model::{Op, Task, TaskList};
use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

pub enum Req {
    Exec(Op),
    Refresh,
    /// Credentials changed on disk (after `ptask auth`).
    Reload,
}

pub enum Created {
    List(TaskList),
    Task(Task),
}

pub enum Resp {
    Done(Option<Created>),
    Failed(ApiError),
    Snapshot(Vec<TaskList>, HashMap<String, Vec<Task>>),
    RefreshFailed(ApiError),
}

fn exec(api: &mut Api, op: &Op) -> Result<Option<Created>, ApiError> {
    let gone = |r: Result<(), ApiError>| match r {
        Err(ApiError::Http(404 | 410, _)) => Ok(None), // already deleted elsewhere
        r => r.map(|_| None),
    };
    match op {
        Op::CreateList { title, .. } => api.create_list(title).map(|l| Some(Created::List(l))),
        Op::RenameList { list, title } => api.rename_list(list, title).map(|_| None),
        Op::DeleteList { list } => gone(api.delete_list(list)),
        Op::CreateTask { list, parent, title, .. } => {
            api.create_task(list, parent.as_deref(), title).map(|t| Some(Created::Task(t)))
        }
        Op::PatchTask { list, task, patch } => api.patch_task(list, task, patch).map(|_| None),
        Op::DeleteTask { list, task } => gone(api.delete_task(list, task)),
        Op::MoveTask { list, task, dest, parent } => api.move_task(list, task, dest, parent.as_deref()).map(|_| None),
    }
}

fn snapshot(api: &mut Api) -> Result<Resp, ApiError> {
    let lists = api.lists()?;
    let mut tasks = HashMap::new();
    for l in &lists {
        tasks.insert(l.id.clone(), api.tasks(&l.id)?);
    }
    Ok(Resp::Snapshot(lists, tasks))
}

pub fn spawn() -> (Sender<Req>, Receiver<Resp>) {
    let (req_tx, req_rx) = channel::<Req>();
    let (resp_tx, resp_rx) = channel();
    thread::spawn(move || {
        let mut api: Option<Api> = None;
        for req in req_rx {
            if matches!(req, Req::Reload) {
                api = None;
                continue;
            }
            if api.is_none() {
                match Api::new() {
                    Ok(a) => api = Some(a),
                    Err(e) => {
                        let err = ApiError::Auth(e);
                        let r = match req {
                            Req::Exec(_) => Resp::Failed(err),
                            _ => Resp::RefreshFailed(err),
                        };
                        let _ = resp_tx.send(r);
                        continue;
                    }
                }
            }
            let a = api.as_mut().unwrap();
            let r = match req {
                Req::Exec(op) => exec(a, &op).map_or_else(Resp::Failed, Resp::Done),
                Req::Refresh => snapshot(a).unwrap_or_else(Resp::RefreshFailed),
                Req::Reload => unreachable!(),
            };
            if resp_tx.send(r).is_err() {
                break;
            }
        }
    });
    (req_tx, resp_rx)
}
