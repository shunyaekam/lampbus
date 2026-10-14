//! Time-of-day schedules: "at 07:00 on weekdays, play the sunrise".
//!
//! The scheduling decision is a pure function of the wall clock (tested); the only I/O is asking the operating system
//! for the local time, by running `date`, which is correct across time zones and daylight-saving changes without
//! pulling a time-zone database into the binary. The runner sleeps until the next scheduled minute (never longer than
//! an hour, so it re-synchronises after clock changes) and wakes at no other time, so an idle daemon stays idle.

use crate::control::{commands, control, Ctx};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

/// One schedule entry.
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    /// Minutes since midnight.
    pub minute: u32,
    /// Weekdays it runs on, `1` = Monday .. `7` = Sunday.
    pub days: Vec<u32>,
    pub control: Value,
}

/// Local wall-clock time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clock {
    /// `1` = Monday .. `7` = Sunday.
    pub weekday: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

pub fn parse_time(s: &str) -> Result<u32, String> {
    let (h, m) = s
        .split_once(':')
        .ok_or_else(|| format!("time {s:?} must look like 07:30"))?;
    let (h, m): (u32, u32) = (
        h.parse().map_err(|_| format!("bad hour in {s:?}"))?,
        m.parse().map_err(|_| format!("bad minute in {s:?}"))?,
    );
    if h > 23 || m > 59 {
        return Err(format!("time {s:?} is out of range"));
    }
    Ok(h * 60 + m)
}

pub fn parse_days(v: Option<&Value>) -> Result<Vec<u32>, String> {
    let Some(v) = v else {
        return Ok((1..=7).collect());
    };
    let arr = v
        .as_array()
        .ok_or("days must be a list like [\"mon\", \"tue\"]")?;
    let mut out = Vec::new();
    for d in arr {
        let name = d
            .as_str()
            .ok_or("days must be strings")?
            .to_ascii_lowercase();
        let n = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"]
            .iter()
            .position(|x| name.starts_with(x))
            .ok_or_else(|| format!("unknown day {name:?}"))?;
        out.push(n as u32 + 1);
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

/// Seconds from `now` until the next time any entry is due (strictly in the future), looking one week ahead.
pub fn seconds_until_next(entries: &[Entry], now: Clock) -> Option<u32> {
    let now_s = now.hour * 3600 + now.minute * 60 + now.second;
    entries
        .iter()
        .flat_map(|e| e.days.iter().map(move |d| (e, *d)))
        .map(|(e, day)| {
            let ahead_days = (day + 7 - now.weekday) % 7;
            let at = ahead_days * 86_400 + e.minute * 60;
            if at > now_s {
                at - now_s
            } else {
                at + 7 * 86_400 - now_s
            }
        })
        .min()
}

/// Entries due in the minute `now` falls in.
pub fn due(entries: &[Entry], now: Clock) -> Vec<usize> {
    let minute = now.hour * 60 + now.minute;
    entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.minute == minute && e.days.contains(&now.weekday))
        .map(|(i, _)| i)
        .collect()
}

async fn local_clock() -> Option<Clock> {
    let out = tokio::task::spawn_blocking(|| {
        std::process::Command::new("date")
            .arg("+%u %H %M %S")
            .output()
    })
    .await
    .ok()?
    .ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    let mut it = text.split_whitespace().map(|x| x.parse::<u32>().ok());
    Some(Clock {
        weekday: it.next()??,
        hour: it.next()??,
        minute: it.next()??,
        second: it.next()??,
    })
}

/// Validate entries up front so a typo is reported at startup, not at 07:00.
pub fn check(ctx: &Ctx, entries: &[Entry]) -> Result<(), String> {
    for e in entries {
        commands(&ctx.library, &e.control)
            .map_err(|err| format!("schedule {:?}: {err}", e.name))?;
    }
    Ok(())
}

pub async fn run(ctx: std::sync::Arc<Ctx>, entries: Vec<Entry>) {
    if entries.is_empty() {
        return;
    }
    let mut fired: HashMap<usize, (u32, u32)> = HashMap::new(); // entry -> (weekday, minute) it last ran
    loop {
        let Some(now) = local_clock().await else {
            tokio::time::sleep(Duration::from_secs(60)).await;
            continue;
        };
        for i in due(&entries, now) {
            let key = (now.weekday, now.hour * 60 + now.minute);
            if fired.get(&i) != Some(&key) {
                fired.insert(i, key);
                if let Some(e) = entries.get(i) {
                    match control(&ctx, &e.control).await {
                        Ok(_) => eprintln!("lampbus: schedule {:?} ran", e.name),
                        Err(err) => eprintln!("lampbus: schedule {:?} failed: {err}", e.name),
                    }
                }
            }
        }
        // sleep to just after the next due minute starts; re-sync with the real clock at least hourly
        let wait = seconds_until_next(&entries, now).map_or(3600, |s| u64::from(s.min(3600)) + 1);
        tokio::time::sleep(Duration::from_secs(wait)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(at: &str, days: &[&str]) -> Entry {
        let list = Value::Array(days.iter().map(|x| json!(x)).collect());
        Entry {
            name: "t".into(),
            minute: parse_time(at).unwrap(),
            days: parse_days(if days.is_empty() { None } else { Some(&list) }).unwrap(),
            control: json!({"power": true}),
        }
    }

    fn at(weekday: u32, h: u32, m: u32, s: u32) -> Clock {
        Clock {
            weekday,
            hour: h,
            minute: m,
            second: s,
        }
    }

    #[test]
    fn time_and_day_parsing() {
        assert_eq!(parse_time("07:30"), Ok(450));
        assert_eq!(parse_time("0:05"), Ok(5));
        for bad in ["7", "24:00", "12:60", "ab:cd", ""] {
            assert!(parse_time(bad).is_err(), "{bad}");
        }
        assert_eq!(parse_days(None).unwrap(), vec![1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(
            parse_days(Some(&json!(["Mon", "friday", "mon"]))).unwrap(),
            vec![1, 5]
        );
        assert!(parse_days(Some(&json!(["funday"]))).is_err());
        assert!(parse_days(Some(&json!("mon"))).is_err());
    }

    #[test]
    fn next_event_distance() {
        let e = [entry("07:00", &["mon", "tue", "wed", "thu", "fri"])];
        assert_eq!(
            seconds_until_next(&e, at(1, 6, 0, 0)),
            Some(3600),
            "later today"
        );
        assert_eq!(
            seconds_until_next(&e, at(1, 7, 0, 0)),
            Some(86_400),
            "exactly now means the next one"
        );
        assert_eq!(
            seconds_until_next(&e, at(5, 8, 0, 0)),
            Some(2 * 86_400 + 23 * 3600),
            "Friday 08:00 -> Monday 07:00"
        );
        assert_eq!(
            seconds_until_next(&e, at(7, 12, 0, 0)),
            Some(19 * 3600),
            "Sunday noon -> Monday 07:00"
        );
        let two = [entry("07:00", &[]), entry("23:00", &[])];
        assert_eq!(seconds_until_next(&two, at(3, 22, 59, 30)), Some(30));
        assert_eq!(seconds_until_next(&[], at(1, 0, 0, 0)), None);
    }

    #[test]
    fn what_is_due_this_minute() {
        let e = [
            entry("07:00", &["mon"]),
            entry("07:00", &["tue"]),
            entry("07:01", &[]),
        ];
        assert_eq!(due(&e, at(1, 7, 0, 12)), vec![0]);
        assert_eq!(due(&e, at(2, 7, 0, 59)), vec![1]);
        assert_eq!(due(&e, at(2, 7, 1, 0)), vec![2]);
        assert!(due(&e, at(3, 7, 0, 0)).is_empty());
    }

    #[test]
    fn every_future_moment_is_reachable_within_a_week() {
        let e = [entry("13:37", &["wed"])];
        for wd in 1..=7 {
            for h in [0, 6, 13, 23] {
                let s = seconds_until_next(&e, at(wd, h, 0, 0)).unwrap();
                assert!(s > 0 && s <= 7 * 86_400, "{wd} {h}: {s}");
            }
        }
    }
}
