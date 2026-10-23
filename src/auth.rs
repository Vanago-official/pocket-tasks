//! OAuth 2.0 for installed apps: loopback redirect + refresh token.

use crate::api::ApiError;
use crate::config::{config_dir, home, write_atomic};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

const SCOPE: &str = "https://www.googleapis.com/auth/tasks";

fn default_auth_uri() -> String {
    "https://accounts.google.com/o/oauth2/auth".into()
}
fn default_token_uri() -> String {
    "https://oauth2.googleapis.com/token".into()
}

#[derive(Deserialize, Clone)]
pub struct Secrets {
    client_id: String,
    client_secret: String,
    #[serde(default = "default_auth_uri")]
    auth_uri: String,
    #[serde(default = "default_token_uri")]
    token_uri: String,
}

#[derive(Deserialize)]
struct SecretsFile {
    installed: Option<Secrets>,
    web: Option<Secrets>,
}

/// Older GTask app kept its credentials here; reuse them so no re-setup is needed.
fn legacy_dir() -> PathBuf {
    home().join(".gtask")
}

fn token_path() -> PathBuf {
    config_dir().join("token.json")
}

pub fn load_secrets() -> Result<Secrets, String> {
    for p in [config_dir().join("client_secrets.json"), legacy_dir().join("client_secrets.json")] {
        if let Ok(s) = std::fs::read_to_string(&p) {
            let f: SecretsFile = serde_json::from_str(&s).map_err(|e| format!("{}: {e}", p.display()))?;
            return f
                .installed
                .or(f.web)
                .ok_or_else(|| format!("{}: немає секції \"installed\"", p.display()));
        }
    }
    Err(format!(
        "Не знайдено client_secrets.json. Створіть OAuth Client ID (Desktop app) у Google Cloud Console \
         з увімкненим Tasks API і покладіть файл у {}",
        config_dir().display()
    ))
}

#[derive(Serialize, Deserialize, Default, Clone)]
struct Token {
    #[serde(default)]
    access_token: String,
    refresh_token: String,
    /// Unix seconds; missing in the legacy file, which forces a refresh.
    #[serde(default)]
    expires_at: i64,
}

fn load_token() -> Option<Token> {
    [token_path(), legacy_dir().join("token.json")]
        .iter()
        .find_map(|p| serde_json::from_str::<Token>(&std::fs::read_to_string(p).ok()?).ok())
        .filter(|t| !t.refresh_token.is_empty())
}

pub fn has_token() -> bool {
    load_token().is_some()
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn save_token(t: &Token) -> Result<(), String> {
    let data = serde_json::to_vec_pretty(t).unwrap();
    write_atomic(&token_path(), &data, true).map_err(|e| format!("Не вдалося зберегти токен: {e}"))
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: i64,
    refresh_token: Option<String>,
}

fn token_request(agent: &ureq::Agent, uri: &str, form: &[(&str, &str)]) -> Result<TokenResponse, ApiError> {
    match agent.post(uri).send_form(form) {
        Ok(r) => r.into_json().map_err(|e| ApiError::Net(e.to_string())),
        Err(ureq::Error::Status(code, r)) if code < 500 => {
            let body = r.into_string().unwrap_or_default();
            Err(ApiError::Auth(format!("Google відхилив токен ({code}): {}", body.trim())))
        }
        Err(e) => Err(ApiError::Net(e.to_string())),
    }
}

pub struct Auth {
    secrets: Secrets,
    token: Token,
}

impl Auth {
    pub fn load() -> Result<Self, String> {
        let secrets = load_secrets()?;
        let token = load_token().ok_or("Немає входу в Google: запустіть `ptask auth`")?;
        Ok(Self { secrets, token })
    }

    pub fn access_token(&mut self, agent: &ureq::Agent) -> Result<String, ApiError> {
        if self.token.access_token.is_empty() || now() >= self.token.expires_at - 60 {
            self.refresh(agent)?;
        }
        Ok(self.token.access_token.clone())
    }

    /// Force a refresh on the next request (after a 401).
    pub fn invalidate(&mut self) {
        self.token.expires_at = 0;
    }

    fn refresh(&mut self, agent: &ureq::Agent) -> Result<(), ApiError> {
        let r = token_request(
            agent,
            &self.secrets.token_uri,
            &[
                ("grant_type", "refresh_token"),
                ("refresh_token", &self.token.refresh_token),
                ("client_id", &self.secrets.client_id),
                ("client_secret", &self.secrets.client_secret),
            ],
        )?;
        self.token.access_token = r.access_token;
        self.token.expires_at = now() + r.expires_in;
        if let Some(rt) = r.refresh_token {
            self.token.refresh_token = rt;
        }
        save_token(&self.token).map_err(ApiError::Auth)
    }
}

fn respond(stream: &mut std::net::TcpStream, msg: &str) {
    let body = format!(
        "<!doctype html><meta charset=utf-8><title>Pocket Tasks</title>\
         <body style=\"font:20px sans-serif;text-align:center;padding-top:20vh\">{msg}</body>"
    );
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

/// Interactive browser login; stores the refresh token in ~/.config/pocket-tasks/token.json.
pub fn login() -> Result<(), String> {
    let secrets = load_secrets()?;
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let redirect = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let state = format!("{:x}{:x}", now(), std::process::id());
    let url = url::Url::parse_with_params(
        &secrets.auth_uri,
        &[
            ("client_id", secrets.client_id.as_str()),
            ("redirect_uri", &redirect),
            ("response_type", "code"),
            ("scope", SCOPE),
            ("access_type", "offline"),
            ("prompt", "consent"),
            ("state", &state),
        ],
    )
    .map_err(|e| e.to_string())?;

    println!("Відкриваю браузер для входу в Google. Якщо не відкрився, перейдіть за посиланням:\n\n{url}\n");
    let _ = Command::new("xdg-open")
        .arg(url.as_str())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    let code = loop {
        let (mut stream, _) = listener.accept().map_err(|e| e.to_string())?;
        let mut line = String::new();
        let _ = BufReader::new(&stream).read_line(&mut line);
        let path = line.split_whitespace().nth(1).unwrap_or("/");
        let Ok(u) = url::Url::parse(&format!("http://localhost{path}")) else { continue };
        let q: HashMap<String, String> = u.query_pairs().into_owned().collect();
        if let Some(err) = q.get("error") {
            respond(&mut stream, "Вхід скасовано.");
            return Err(format!("Google повернув помилку: {err}"));
        }
        match q.get("code") {
            Some(_) if q.get("state") != Some(&state) => respond(&mut stream, "Невірний state, спробуйте ще раз."),
            Some(code) => {
                respond(&mut stream, "✅ Готово! Вкладку можна закрити.");
                break code.clone();
            }
            None => respond(&mut stream, ""), // favicon etc.
        }
    };

    let agent = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(20)).build();
    let r = token_request(
        &agent,
        &secrets.token_uri,
        &[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("client_id", &secrets.client_id),
            ("client_secret", &secrets.client_secret),
            ("redirect_uri", &redirect),
        ],
    )
    .map_err(|e| e.to_string())?;
    let refresh_token = r.refresh_token.ok_or("Google не повернув refresh_token")?;
    save_token(&Token { access_token: r.access_token, refresh_token, expires_at: now() + r.expires_in })?;
    println!("Вхід виконано.");
    Ok(())
}
