//! Thin blocking client for the Google Tasks REST API v1.

use crate::auth::Auth;
use crate::model::{Task, TaskList};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::fmt;
use std::time::Duration;

const BASE: &str = "https://tasks.googleapis.com/tasks/v1";

#[derive(Debug, Clone)]
pub enum ApiError {
    /// Network trouble; worth retrying later.
    Net(String),
    Http(u16, String),
    /// Refresh token rejected; the user has to log in again.
    Auth(String),
}

impl ApiError {
    pub fn is_transient(&self) -> bool {
        match self {
            ApiError::Net(_) => true,
            ApiError::Http(c, _) => *c == 429 || *c >= 500,
            ApiError::Auth(_) => false,
        }
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ApiError::Net(e) => write!(f, "мережа: {e}"),
            ApiError::Http(c, e) => write!(f, "HTTP {c}: {e}"),
            ApiError::Auth(e) => write!(f, "{e}"),
        }
    }
}

pub struct Api {
    agent: ureq::Agent,
    auth: Auth,
}

fn list_url(list: &str) -> String {
    format!("{BASE}/users/@me/lists/{list}")
}
fn tasks_url(list: &str) -> String {
    format!("{BASE}/lists/{list}/tasks")
}

impl Api {
    pub fn new() -> Result<Self, String> {
        let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(15)).build();
        Ok(Self { agent, auth: Auth::load()? })
    }

    fn req(&mut self, method: &str, url: &str, body: Option<&Value>) -> Result<Value, ApiError> {
        for attempt in 0..2 {
            let token = self.auth.access_token(&self.agent)?;
            let r = self.agent.request(method, url).set("Authorization", &format!("Bearer {token}"));
            let res = match body {
                Some(b) => r.send_json(b),
                // Google answers 411 to a POST without Content-Length
                None if method == "POST" => r.send_string(""),
                None => r.call(),
            };
            match res {
                Ok(resp) => {
                    let s = resp.into_string().map_err(|e| ApiError::Net(e.to_string()))?;
                    if s.trim().is_empty() {
                        return Ok(Value::Null);
                    }
                    return serde_json::from_str(&s).map_err(|e| ApiError::Net(e.to_string()));
                }
                Err(ureq::Error::Status(401, _)) if attempt == 0 => self.auth.invalidate(),
                Err(ureq::Error::Status(code, resp)) => {
                    let msg = resp
                        .into_json::<Value>()
                        .ok()
                        .and_then(|v| v["error"]["message"].as_str().map(String::from))
                        .unwrap_or_default();
                    return Err(ApiError::Http(code, msg));
                }
                Err(e) => return Err(ApiError::Net(e.to_string())),
            }
        }
        Err(ApiError::Auth("Google не приймає токен: увійдіть знову".into()))
    }

    fn list_all<T: DeserializeOwned>(&mut self, base: &str, extra: &[(&str, &str)]) -> Result<Vec<T>, ApiError> {
        let mut out = Vec::new();
        let mut page: Option<String> = None;
        loop {
            let mut u = url::Url::parse(base).expect("valid url");
            {
                let mut q = u.query_pairs_mut();
                q.append_pair("maxResults", "100");
                for (k, v) in extra {
                    q.append_pair(k, v);
                }
                if let Some(p) = &page {
                    q.append_pair("pageToken", p);
                }
            }
            let v = self.req("GET", u.as_str(), None)?;
            if let Some(items) = v.get("items") {
                out.extend(serde_json::from_value::<Vec<T>>(items.clone()).map_err(|e| ApiError::Net(e.to_string()))?);
            }
            page = v.get("nextPageToken").and_then(Value::as_str).map(String::from);
            if page.is_none() {
                return Ok(out);
            }
        }
    }

    fn parse<T: DeserializeOwned>(v: Value) -> Result<T, ApiError> {
        serde_json::from_value(v).map_err(|e| ApiError::Net(e.to_string()))
    }

    pub fn lists(&mut self) -> Result<Vec<TaskList>, ApiError> {
        self.list_all(&format!("{BASE}/users/@me/lists"), &[])
    }

    pub fn tasks(&mut self, list: &str) -> Result<Vec<Task>, ApiError> {
        // Tasks completed in Google's own apps are "hidden", so ask for them too.
        let mut v: Vec<Task> =
            self.list_all(&tasks_url(list), &[("showCompleted", "true"), ("showHidden", "true")])?;
        v.retain(|t| !t.deleted);
        Ok(v)
    }

    pub fn create_list(&mut self, title: &str) -> Result<TaskList, ApiError> {
        let v = self.req("POST", &format!("{BASE}/users/@me/lists"), Some(&serde_json::json!({ "title": title })))?;
        Self::parse(v)
    }

    pub fn rename_list(&mut self, list: &str, title: &str) -> Result<(), ApiError> {
        self.req("PATCH", &list_url(list), Some(&serde_json::json!({ "title": title }))).map(drop)
    }

    pub fn delete_list(&mut self, list: &str) -> Result<(), ApiError> {
        self.req("DELETE", &list_url(list), None).map(drop)
    }

    pub fn create_task(&mut self, list: &str, parent: Option<&str>, title: &str) -> Result<Task, ApiError> {
        let mut u = url::Url::parse(&tasks_url(list)).expect("valid url");
        if let Some(p) = parent {
            u.query_pairs_mut().append_pair("parent", p);
        }
        let v = self.req("POST", u.as_str(), Some(&serde_json::json!({ "title": title })))?;
        Self::parse(v)
    }

    pub fn patch_task(&mut self, list: &str, task: &str, patch: &Value) -> Result<(), ApiError> {
        self.req("PATCH", &format!("{}/{task}", tasks_url(list)), Some(patch)).map(drop)
    }

    /// Google keeps the task id and moves its subtasks along; no `parent` means top level.
    pub fn move_task(&mut self, list: &str, task: &str, dest: &str, parent: Option<&str>) -> Result<(), ApiError> {
        let mut u = url::Url::parse(&format!("{}/{task}/move", tasks_url(list))).expect("valid url");
        {
            let mut q = u.query_pairs_mut();
            if dest != list {
                q.append_pair("destinationTasklist", dest);
            }
            if let Some(p) = parent {
                q.append_pair("parent", p);
            }
        }
        self.req("POST", u.as_str(), None).map(drop)
    }

    pub fn delete_task(&mut self, list: &str, task: &str) -> Result<(), ApiError> {
        self.req("DELETE", &format!("{}/{task}", tasks_url(list)), None).map(drop)
    }
}
