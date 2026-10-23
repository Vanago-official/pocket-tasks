mod api;
mod app;
mod auth;
mod config;
mod date;
mod model;
mod sync;
mod ui;

use crossterm::event::{
    self, DisableFocusChange, DisableMouseCapture, EnableFocusChange, EnableMouseCapture, Event, KeyEventKind,
};
use crossterm::execute;
use std::io::{stdout, BufRead};
use std::time::Duration;

const USAGE: &str = "\
ptask — кишеньковий таск-трекер з синхронізацією Google Tasks

  ptask          відкрити трекер
  ptask auth     увійти в Google
  ptask waybar   JSON для модуля Waybar (завдання на сьогодні та прострочені)
";

fn wait_enter() {
    println!("\nНатисніть Enter…");
    let _ = std::io::stdin().lock().read_line(&mut String::new());
}

fn main() {
    match std::env::args().nth(1).as_deref() {
        None => run_tui(),
        Some("auth" | "login") => {
            if let Err(e) = auth::login() {
                eprintln!("Помилка: {e}");
                std::process::exit(1);
            }
        }
        Some("waybar") => waybar(),
        Some("-h" | "--help" | "help") => print!("{USAGE}"),
        Some(other) => {
            eprintln!("Невідома команда: {other}\n\n{USAGE}");
            std::process::exit(2);
        }
    }
}

fn run_tui() {
    if !auth::has_token() {
        println!("Потрібен вхід у Google.\n");
        if let Err(e) = auth::login() {
            eprintln!("Помилка: {e}");
            wait_enter();
            std::process::exit(1);
        }
    }
    let (tx, rx) = sync::spawn();
    let pocket = std::env::var_os("PTASK_POCKET").is_some();
    let mut app = app::App::new(model::State::load(), tx, pocket);

    let mut term = ratatui::init();
    let _ = execute!(stdout(), EnableFocusChange, EnableMouseCapture);
    let res = (|| -> std::io::Result<()> {
        while !app.quit {
            while let Ok(r) = rx.try_recv() {
                app.on_resp(r);
            }
            app.tick();
            app.save_if_dirty();
            term.draw(|f| ui::draw(f, &mut app))?;
            if event::poll(Duration::from_millis(250))? {
                match event::read()? {
                    Event::Key(k) if k.kind == KeyEventKind::Press => app.on_key(k),
                    Event::Mouse(m) => app.on_mouse(m),
                    Event::FocusGained => app.on_focus(),
                    _ => {}
                }
            }
            if app.login_requested {
                app.login_requested = false;
                let _ = execute!(stdout(), DisableFocusChange, DisableMouseCapture);
                ratatui::restore();
                if let Err(e) = auth::login() {
                    eprintln!("Помилка: {e}");
                    wait_enter();
                }
                term = ratatui::init();
                let _ = execute!(stdout(), EnableFocusChange, EnableMouseCapture);
                app.reauth();
            }
        }
        Ok(())
    })();
    let _ = execute!(stdout(), DisableFocusChange, DisableMouseCapture);
    ratatui::restore();
    app.save();
    if let Err(e) = res {
        eprintln!("{e}");
    }
}

/// Reads only the local cache, so it is instant and works offline.
fn waybar() {
    let st = model::State::load();
    let today = date::today();
    let (mut overdue, mut due_today) = (Vec::new(), Vec::new());
    for l in &st.lists {
        for t in st.tasks.get(&l.id).into_iter().flatten() {
            match t.due_date() {
                Some(d) if !t.done() && d < today => overdue.push(format!("⚠ {} · {}", t.title, date::human(d, today))),
                Some(d) if !t.done() && d == today => due_today.push(format!("• {}", t.title)),
                _ => {}
            }
        }
    }
    let n = overdue.len() + due_today.len();
    let class = if !overdue.is_empty() { "overdue" } else if n > 0 { "today" } else { "none" };
    let text = if n > 0 { format!("󰄲 {n}") } else { "󰄲".into() };
    let tooltip = if n == 0 {
        "На сьогодні завдань немає".to_string()
    } else {
        overdue.into_iter().chain(due_today).collect::<Vec<_>>().join("\n")
    };
    println!("{}", serde_json::json!({ "text": text, "tooltip": tooltip, "class": class }));
}
