use chrono::{Datelike, Duration, Local, NaiveDate, Weekday};

pub fn today() -> NaiveDate {
    Local::now().date_naive()
}

/// Google Tasks keeps only the date part of `due` ("2026-10-23T00:00:00.000Z").
pub fn parse_api(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.get(..10)?, "%Y-%m-%d").ok()
}

pub fn to_api(d: NaiveDate) -> String {
    format!("{}T00:00:00.000Z", d.format("%Y-%m-%d"))
}

fn weekday(s: &str) -> Option<Weekday> {
    Some(match s {
        "пн" | "mon" | "mo" => Weekday::Mon,
        "вт" | "tue" | "tu" => Weekday::Tue,
        "ср" | "wed" | "we" => Weekday::Wed,
        "чт" | "thu" | "th" => Weekday::Thu,
        "пт" | "fri" | "fr" => Weekday::Fri,
        "сб" | "sat" | "sa" => Weekday::Sat,
        "нд" | "sun" | "su" => Weekday::Sun,
        _ => return None,
    })
}

/// Parses user input: today/t, tomorrow/tm, +N, weekday (пн..нд / mon..sun),
/// DD.MM, DD.MM.YY(YY), YYYY-MM-DD. Empty or "-" clears the due date.
pub fn parse_input(input: &str, today: NaiveDate) -> Result<Option<NaiveDate>, String> {
    let s = input.trim().to_lowercase();
    let bad = || format!("Не розумію дату «{}»", input.trim());
    let d = match s.as_str() {
        "" | "-" => return Ok(None),
        "t" | "today" | "сьогодні" | "сег" | "с" => today,
        "tm" | "tomorrow" | "завтра" | "з" => today + Duration::days(1),
        "післязавтра" | "пз" => today + Duration::days(2),
        _ => {
            if let Some(n) = s.strip_prefix('+') {
                let (num, mult) = match n.strip_suffix(['w', 'т']) {
                    Some(w) => (w, 7),
                    None => (n.strip_suffix(['d', 'д']).unwrap_or(n), 1),
                };
                let n: i64 = num.parse().map_err(|_| bad())?;
                today + Duration::days(n * mult)
            } else if let Some(wd) = weekday(&s) {
                let mut d = today + Duration::days(1);
                while d.weekday() != wd {
                    d += Duration::days(1);
                }
                d
            } else if let Ok(d) = NaiveDate::parse_from_str(&s, "%Y-%m-%d") {
                d
            } else {
                let parts: Vec<&str> = s.split(['.', '/']).collect();
                let num = |i: usize| parts.get(i).and_then(|p| p.parse::<u32>().ok());
                match parts.len() {
                    2 => {
                        let (dd, mm) = (num(0).ok_or_else(bad)?, num(1).ok_or_else(bad)?);
                        let d = NaiveDate::from_ymd_opt(today.year(), mm, dd).ok_or_else(bad)?;
                        // "05.01" typed in December means next January
                        if d < today {
                            NaiveDate::from_ymd_opt(today.year() + 1, mm, dd).ok_or_else(bad)?
                        } else {
                            d
                        }
                    }
                    3 => {
                        let mut y = num(2).ok_or_else(bad)? as i32;
                        if y < 100 {
                            y += 2000;
                        }
                        NaiveDate::from_ymd_opt(y, num(1).ok_or_else(bad)?, num(0).ok_or_else(bad)?)
                            .ok_or_else(bad)?
                    }
                    _ => return Err(bad()),
                }
            }
        }
    };
    Ok(Some(d))
}

const WD: [&str; 7] = ["пн", "вт", "ср", "чт", "пт", "сб", "нд"];

/// Short label for task rows.
pub fn human(d: NaiveDate, today: NaiveDate) -> String {
    match (d - today).num_days() {
        0 => "сьогодні".into(),
        1 => "завтра".into(),
        -1 => "вчора".into(),
        2..=6 => WD[d.weekday().num_days_from_monday() as usize].into(),
        _ if d.year() == today.year() => d.format("%d.%m").to_string(),
        _ => d.format("%d.%m.%y").to_string(),
    }
}

/// Full label for the details pane.
pub fn long(d: NaiveDate) -> String {
    format!("{}, {}", WD[d.weekday().num_days_from_monday() as usize], d.format("%d.%m.%Y"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_inputs() {
        let t = NaiveDate::from_ymd_opt(2026, 10, 23).unwrap(); // Friday
        let p = |s| parse_input(s, t).unwrap().map(|d| d.to_string());
        assert_eq!(p(""), None);
        assert_eq!(p("завтра").unwrap(), "2026-10-24");
        assert_eq!(p("+3").unwrap(), "2026-10-26");
        assert_eq!(p("+2w").unwrap(), "2026-11-06");
        assert_eq!(p("пт").unwrap(), "2026-10-30");
        assert_eq!(p("пн").unwrap(), "2026-10-26");
        assert_eq!(p("25.10").unwrap(), "2026-10-25");
        assert_eq!(p("05.01").unwrap(), "2027-01-05");
        assert_eq!(p("1.2.27").unwrap(), "2027-02-01");
        assert_eq!(p("2026-12-31").unwrap(), "2026-12-31");
        assert!(parse_input("абв", t).is_err());
        assert_eq!(parse_api("2026-10-23T00:00:00.000Z"), Some(t));
        assert_eq!(to_api(t), "2026-10-23T00:00:00.000Z");
    }
}
