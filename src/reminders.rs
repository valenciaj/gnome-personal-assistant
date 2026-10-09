//! Cuándo toca avisar de cada tarea o recordatorio.

use chrono::{Days, Local, Months, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};

use crate::store::{Repeat, Todo};

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

/// Siguiente aviso de un recordatorio que se repite: la primera repetición de
/// `anchor` (en hora local) posterior a `after`. `None` si no se repite.
///
/// Se cuenta siempre desde la fecha original para que un día 31 no se convierta
/// para siempre en 28 tras pasar por febrero.
pub fn next_occurrence(anchor: i64, repeat: Repeat, after: i64) -> Option<i64> {
    let base = chrono::DateTime::from_timestamp(anchor, 0)?.with_timezone(&Local).naive_local();
    let step = |n: u32| -> Option<NaiveDateTime> {
        match repeat {
            Repeat::None => None,
            Repeat::Daily => base.checked_add_days(Days::new(u64::from(n))),
            Repeat::Weekly => base.checked_add_days(Days::new(7 * u64::from(n))),
            Repeat::Monthly => base.checked_add_months(Months::new(n)),
            Repeat::Yearly => base.checked_add_months(Months::new(12 * n)),
        }
    };
    // Saltar directamente cerca de `after` para no iterar años de días.
    let elapsed_days = ((after - anchor).max(0) / 86_400) as u32;
    let start = match repeat {
        Repeat::None => return None,
        Repeat::Daily => elapsed_days.saturating_sub(2),
        Repeat::Weekly => (elapsed_days / 7).saturating_sub(1),
        Repeat::Monthly => (elapsed_days / 31).saturating_sub(1),
        Repeat::Yearly => (elapsed_days / 366).saturating_sub(1),
    };
    (start.max(1)..start + 400).find_map(|n| {
        let candidate = Local.from_local_datetime(&step(n)?).earliest()?.timestamp();
        (candidate > after).then_some(candidate)
    })
}

/// Una opción de posponer: "10m", "2h", "1d" o "tomorrow" (mañana a `default_time`).
pub fn snooze_until(option: &str, now: i64, default_time: NaiveTime) -> Option<i64> {
    let option = option.trim();
    if option == "tomorrow" {
        let today = chrono::DateTime::from_timestamp(now, 0)?.with_timezone(&Local).date_naive();
        let tomorrow = today.succ_opt()?.and_time(default_time);
        return Local.from_local_datetime(&tomorrow).earliest().map(|t| t.timestamp());
    }
    let (number, unit) = option.split_at(option.len().checked_sub(1)?);
    let amount: i64 = number.parse().ok().filter(|n| *n > 0)?;
    let seconds = match unit {
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        _ => return None,
    };
    Some(now + amount * seconds)
}

/// Texto de una opción de posponer, para botones y menús.
pub fn snooze_label(option: &str, default_time: NaiveTime) -> String {
    use crate::i18n::trf;
    let option = option.trim();
    if option == "tomorrow" {
        return trf("Mañana a las {time}", &[("time", &default_time.format("%H:%M"))]);
    }
    let (number, unit) = option.split_at(option.len().saturating_sub(1));
    match unit {
        "m" => trf("{n} min", &[("n", &number)]),
        "h" => trf("{n} h", &[("n", &number)]),
        "d" => trf("{n} días", &[("n", &number)]),
        _ => option.to_owned(),
    }
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

    #[test]
    fn snooze_options() {
        let nine = parse_default_time("09:00");
        assert_eq!(snooze_until("10m", 1000, nine), Some(1600));
        assert_eq!(snooze_until("2h", 0, nine), Some(7200));
        assert_eq!(snooze_until("x", 0, nine), None);
        assert_eq!(snooze_until("0m", 0, nine), None);
        let now = Local.from_local_datetime(&NaiveDateTime::parse_from_str("2026-03-10 22:00", "%Y-%m-%d %H:%M").unwrap()).unwrap();
        let expected = Local.from_local_datetime(&NaiveDateTime::parse_from_str("2026-03-11 09:00", "%Y-%m-%d %H:%M").unwrap()).unwrap();
        assert_eq!(snooze_until("tomorrow", now.timestamp(), nine), Some(expected.timestamp()));
    }

    #[test]
    fn repetitions() {
        let local = |s: &str| {
            Local.from_local_datetime(&NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()).unwrap().timestamp()
        };
        let jan31 = local("2026-01-31 08:00");
        assert_eq!(next_occurrence(jan31, Repeat::None, jan31), None);
        assert_eq!(next_occurrence(jan31, Repeat::Daily, jan31), Some(local("2026-02-01 08:00")));
        assert_eq!(next_occurrence(jan31, Repeat::Weekly, local("2026-02-01 00:00")), Some(local("2026-02-07 08:00")));
        // Febrero no tiene 31: se usa el último día, pero marzo vuelve al 31.
        assert_eq!(next_occurrence(jan31, Repeat::Monthly, jan31), Some(local("2026-02-28 08:00")));
        assert_eq!(next_occurrence(jan31, Repeat::Monthly, local("2026-03-01 00:00")), Some(local("2026-03-31 08:00")));
        assert_eq!(next_occurrence(jan31, Repeat::Yearly, local("2026-06-01 00:00")), Some(local("2027-01-31 08:00")));
        // Mucho después de la fecha original.
        assert_eq!(next_occurrence(jan31, Repeat::Daily, local("2030-05-10 09:00")), Some(local("2030-05-11 08:00")));
    }
}
