//! Cuándo toca avisar de cada tarea.

use chrono::{Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};

use crate::store::Todo;

/// Avisos perdidos (la app estaba cerrada) con más antigüedad que esto no se muestran.
pub const MAX_LATE_SECONDS: i64 = 24 * 3600;

/// Momento del aviso: el aviso explícito; si no hay, la fecha límite (a
/// `default_time` si solo tiene día).
pub fn reminder_time(todo: &Todo, default_time: NaiveTime) -> Option<i64> {
    if let Some(at) = todo.reminder_at {
        return Some(at);
    }
    let due = todo.due.as_deref()?;
    let naive = match NaiveDate::parse_from_str(due, "%Y-%m-%d") {
        Ok(date) => date.and_time(default_time),
        Err(_) => NaiveDateTime::parse_from_str(due, "%Y-%m-%d %H:%M").ok()?,
    };
    Local.from_local_datetime(&naive).earliest().map(|t| t.timestamp())
}

/// 'HH:MM' → hora; si no es válida, las 9:00.
pub fn parse_default_time(text: &str) -> NaiveTime {
    NaiveTime::parse_from_str(text.trim(), "%H:%M").unwrap_or(NaiveTime::from_hms_opt(9, 0, 0).expect("hora válida"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Priority;

    fn todo(due: Option<&str>, reminder_at: Option<i64>) -> Todo {
        Todo {
            id: 1,
            list_id: 1,
            list: "x".into(),
            text: "x".into(),
            done: false,
            due: due.map(str::to_owned),
            priority: Priority::None,
            reminder_at,
            created: 0,
            completed: None,
        }
    }

    #[test]
    fn reminder_times() {
        let nine = parse_default_time("09:00");
        let local = |s: &str| {
            Local.from_local_datetime(&NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()).unwrap().timestamp()
        };
        assert_eq!(reminder_time(&todo(Some("2026-10-10"), None), nine), Some(local("2026-10-10 09:00")));
        assert_eq!(reminder_time(&todo(Some("2026-10-10 18:30"), None), nine), Some(local("2026-10-10 18:30")));
        assert_eq!(reminder_time(&todo(Some("2026-10-10"), Some(42)), nine), Some(42));
        assert_eq!(reminder_time(&todo(None, None), nine), None);
        assert_eq!(parse_default_time("nope"), nine);
    }
}
