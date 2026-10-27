//! How an event repeats: Spark's repeat rule (Every day / week / month /
//! year, or a custom one, ending never, after some times or on a date), as
//! Microsoft 365 keeps it, Graph's `patternedRecurrence`. Graph keeps one
//! day of the month and one month a rule, where Spark lets a custom rule
//! pick several; the editor offers what Graph keeps.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::graph::{civil_from_days, days_from_civil};

/// How often it repeats.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Every {
    Day,
    Week,
    Month,
    Year,
}

/// When it stops repeating.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Ends {
    #[default]
    Never,
    /// After this many times, the first included.
    After { times: u32 },
    /// On this date ("2026-12-31"), the last it can be on.
    On { date: String },
}

/// A repeat rule, as the editor sets it and the UI's `Repeat` mirrors.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Repeat {
    pub every: Every,
    /// Every this many days, weeks, months or years (1 to 999).
    pub interval: u32,
    /// Weekly, the days it's on, 0 (Sunday) to 6; monthly or yearly "on
    /// the second Tuesday", that one day.
    #[serde(default)]
    pub weekdays: Vec<u8>,
    /// Monthly or yearly "on the …" week of the month: 1 to 4, or 5 for the
    /// last. None: on a day of the month (`day`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nth: Option<u8>,
    /// Monthly or yearly on a date: its day of the month, 1 to 31.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub day: Option<u8>,
    /// Yearly: its month, 1 to 12.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub month: Option<u8>,
    #[serde(default)]
    pub ends: Ends,
}

const WEEKDAYS: [&str; 7] = [
    "sunday",
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
];
const NTH: [&str; 5] = ["first", "second", "third", "fourth", "last"];

impl Repeat {
    /// Graph's `recurrence` for this rule, for a series starting on `first`
    /// ("2026-10-27"), its times in the Windows time zone `zone`.
    pub fn to_graph(&self, first: &str, zone: &str) -> Value {
        let interval = self.interval.clamp(1, 999);
        let days: Vec<&str> = self
            .weekdays
            .iter()
            .filter_map(|&d| WEEKDAYS.get(d as usize).copied())
            .collect();
        let index = self
            .nth
            .and_then(|n| NTH.get(n.clamp(1, 5) as usize - 1).copied());
        let pattern = match (self.every, index) {
            (Every::Day, _) => json!({ "type": "daily", "interval": interval }),
            (Every::Week, _) => json!({
                "type": "weekly",
                "interval": interval,
                "daysOfWeek": days,
                "firstDayOfWeek": "sunday",
            }),
            (Every::Month, None) => json!({
                "type": "absoluteMonthly",
                "interval": interval,
                "dayOfMonth": self.day.unwrap_or(1),
            }),
            (Every::Month, Some(index)) => json!({
                "type": "relativeMonthly",
                "interval": interval,
                "daysOfWeek": days,
                "index": index,
            }),
            (Every::Year, None) => json!({
                "type": "absoluteYearly",
                "interval": interval,
                "dayOfMonth": self.day.unwrap_or(1),
                "month": self.month.unwrap_or(1),
            }),
            (Every::Year, Some(index)) => json!({
                "type": "relativeYearly",
                "interval": interval,
                "daysOfWeek": days,
                "index": index,
                "month": self.month.unwrap_or(1),
            }),
        };
        let range = match &self.ends {
            Ends::Never => {
                json!({ "type": "noEnd", "startDate": first, "recurrenceTimeZone": zone })
            }
            Ends::After { times } => json!({
                "type": "numbered",
                "startDate": first,
                "numberOfOccurrences": (*times).max(1),
                "recurrenceTimeZone": zone,
            }),
            Ends::On { date } => json!({
                "type": "endDate",
                "startDate": first,
                "endDate": date,
                "recurrenceTimeZone": zone,
            }),
        };
        json!({ "pattern": pattern, "range": range })
    }

    /// The rule Graph's `recurrence` says, if it's one the editor knows.
    pub fn from_graph(recurrence: &Value) -> Option<Self> {
        let pattern = &recurrence["pattern"];
        let range = &recurrence["range"];
        let interval = pattern["interval"].as_u64().unwrap_or(1).clamp(1, 999) as u32;
        let weekdays: Vec<u8> = pattern["daysOfWeek"]
            .as_array()
            .map(|days| {
                days.iter()
                    .filter_map(|d| {
                        let d = d.as_str()?.to_ascii_lowercase();
                        WEEKDAYS.iter().position(|w| *w == d).map(|i| i as u8)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let nth = pattern["index"]
            .as_str()
            .and_then(|i| NTH.iter().position(|n| *n == i))
            .map(|i| i as u8 + 1);
        let day = pattern["dayOfMonth"]
            .as_u64()
            .filter(|d| (1..=31).contains(d))
            .map(|d| d as u8);
        let month = pattern["month"]
            .as_u64()
            .filter(|m| (1..=12).contains(m))
            .map(|m| m as u8);
        let (every, nth, day, month) = match pattern["type"].as_str()? {
            "daily" => (Every::Day, None, None, None),
            "weekly" => (Every::Week, None, None, None),
            "absoluteMonthly" => (Every::Month, None, day, None),
            "relativeMonthly" => (Every::Month, nth.or(Some(1)), None, None),
            "absoluteYearly" => (Every::Year, None, day, month),
            "relativeYearly" => (Every::Year, nth.or(Some(1)), None, month),
            _ => return None,
        };
        let ends = match range["type"].as_str() {
            Some("numbered") => Ends::After {
                times: range["numberOfOccurrences"].as_u64().unwrap_or(1).max(1) as u32,
            },
            Some("endDate") => match range["endDate"].as_str() {
                Some(date) if date.len() >= 10 && !date.starts_with("0001") => Ends::On {
                    date: date[..10].to_owned(),
                },
                _ => Ends::Never,
            },
            _ => Ends::Never,
        };
        Some(Self {
            every,
            interval,
            weekdays: if every == Every::Week || nth.is_some() {
                weekdays
            } else {
                Vec::new()
            },
            nth,
            day,
            month,
            ends,
        })
    }
}

/// `recurrence` (Graph's, a series') ending on the day before `date`
/// ("2026-10-27"): what's left of a series split there for "All future".
pub fn ended_before(recurrence: &Value, date: &str) -> Value {
    let mut recurrence = recurrence.clone();
    let range = &mut recurrence["range"];
    range["type"] = json!("endDate");
    range["endDate"] = json!(day_before(date));
    if let Some(range) = range.as_object_mut() {
        range.remove("numberOfOccurrences");
    }
    recurrence
}

/// The date before `date`, both "2026-10-27".
pub fn day_before(date: &str) -> String {
    add_days(date, -1)
}

/// `date` ("2026-10-27") as days since 1970-01-01.
pub fn day_number(date: &str) -> Option<i64> {
    let part = |r: std::ops::Range<usize>| date.get(r).and_then(|s| s.parse::<i64>().ok());
    Some(days_from_civil(part(0..4)?, part(5..7)?, part(8..10)?))
}

/// `date` ("2026-10-27") `days` later (or earlier).
pub fn add_days(date: &str, days: i64) -> String {
    let Some(n) = day_number(date) else {
        return date.to_owned();
    };
    let (y, m, d) = civil_from_days(n + days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// The days from `from` to `to`, both "2026-10-27".
pub fn days_between(from: &str, to: &str) -> i64 {
    match (day_number(from), day_number(to)) {
        (Some(a), Some(b)) => b - a,
        _ => 0,
    }
}

/// `date`'s weekday, 0 (Sunday) to 6.
fn weekday(date: &str) -> usize {
    // 1970-01-01 was a Thursday.
    day_number(date).map_or(0, |n| (n + 4).rem_euclid(7) as usize)
}

/// Which week of its month `date` is in, as Graph names it: "first" to
/// "fourth", or "last" when it's the month's last.
fn week_of_month(date: &str) -> &'static str {
    let day = date
        .get(8..10)
        .and_then(|d| d.parse::<usize>().ok())
        .unwrap_or(1);
    let next_week = add_days(date, 7);
    if next_week.get(5..7) != date.get(5..7) {
        "last"
    } else {
        NTH[((day - 1) / 7).min(3)]
    }
}

/// What a rule's pattern says, its end aside, to tell whether only its
/// end changed.
fn pattern(rule: &Repeat) -> Repeat {
    Repeat {
        ends: Ends::Never,
        ..rule.clone()
    }
}

/// What a series' Graph `old` recurrence becomes for a change: `rule` if
/// the editor changed it, else `old` itself. Moved `days` days (the event
/// dragged to another day), a pattern that was the series' own moves with
/// it: weekly days by as many, monthly and yearly to the new first day's
/// date or week. With `start`, the series' first day and time zone moved
/// there; else they stay. What `old` says that a rule doesn't, the week's
/// first day, stays, and so does all of its pattern if only the end
/// changed.
pub fn changed(
    old: &Value,
    rule: Option<&Repeat>,
    days: i64,
    start: Option<(&str, &str)>,
) -> Value {
    let first = start.map_or_else(
        || {
            old["range"]["startDate"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        },
        |(first, _)| first.to_owned(),
    );
    let zone = start.map_or_else(
        || {
            old["range"]["recurrenceTimeZone"]
                .as_str()
                .unwrap_or("UTC")
                .to_owned()
        },
        |(_, zone)| zone.to_owned(),
    );
    let was = Repeat::from_graph(old);
    let own_pattern = match (rule, &was) {
        (None, _) => true,
        (Some(rule), Some(was)) => pattern(rule) == pattern(was),
        (Some(_), None) => false,
    };
    let mut out = match rule {
        Some(rule) if !own_pattern => {
            let mut out = rule.to_graph(&first, &zone);
            if let Some(day) = old["pattern"]
                .get("firstDayOfWeek")
                .filter(|_| rule.every == Every::Week)
            {
                out["pattern"]["firstDayOfWeek"] = day.clone();
            }
            return out;
        }
        Some(rule) => {
            let mut out = old.clone();
            out["range"] = rule.to_graph(&first, &zone)["range"].clone();
            out
        }
        None => {
            let mut out = old.clone();
            out["range"]["startDate"] = json!(first);
            out["range"]["recurrenceTimeZone"] = json!(zone);
            out
        }
    };
    if days != 0 && start.is_some() {
        let p = &mut out["pattern"];
        match p["type"].as_str() {
            Some("weekly") => {
                let shift = |d: &str| {
                    let i = WEEKDAYS.iter().position(|w| *w == d)?;
                    Some(WEEKDAYS[(i as i64 + days).rem_euclid(7) as usize])
                };
                if let Some(list) = p["daysOfWeek"].as_array() {
                    let moved: Vec<&str> = list.iter().filter_map(|d| shift(d.as_str()?)).collect();
                    p["daysOfWeek"] = json!(moved);
                }
                // The week starts as many days later too, so days that were
                // in one week (Friday and Saturday, every other week) still
                // are (Saturday and Sunday).
                let first_day = p["firstDayOfWeek"].as_str().unwrap_or("sunday");
                p["firstDayOfWeek"] = json!(shift(first_day).unwrap_or("sunday"));
            }
            Some("absoluteMonthly") => p["dayOfMonth"] = json!(day_of(&first)),
            Some("relativeMonthly") => {
                p["daysOfWeek"] = json!([WEEKDAYS[weekday(&first)]]);
                p["index"] = json!(week_of_month(&first));
            }
            Some("absoluteYearly") => {
                p["dayOfMonth"] = json!(day_of(&first));
                p["month"] = json!(month_of(&first));
            }
            Some("relativeYearly") => {
                p["daysOfWeek"] = json!([WEEKDAYS[weekday(&first)]]);
                p["index"] = json!(week_of_month(&first));
                p["month"] = json!(month_of(&first));
            }
            _ => {}
        }
    }
    out
}

fn day_of(date: &str) -> u32 {
    date.get(8..10).and_then(|d| d.parse().ok()).unwrap_or(1)
}

fn month_of(date: &str) -> u32 {
    date.get(5..7).and_then(|d| d.parse().ok()).unwrap_or(1)
}

/// `recurrence` ending on `date` (its last occurrence's day) rather than
/// after so many: a series split for "All future" ends where the old one
/// would have.
pub fn ending_on(recurrence: &Value, date: &str) -> Value {
    let mut recurrence = recurrence.clone();
    let range = &mut recurrence["range"];
    range["type"] = json!("endDate");
    range["endDate"] = json!(date);
    if let Some(range) = range.as_object_mut() {
        range.remove("numberOfOccurrences");
    }
    recurrence
}

#[cfg(test)]
mod tests {
    use super::*;

    fn weekly() -> Repeat {
        Repeat {
            every: Every::Week,
            interval: 2,
            weekdays: vec![1, 3],
            nth: None,
            day: None,
            month: None,
            ends: Ends::After { times: 10 },
        }
    }

    #[test]
    fn rules_go_to_graph_and_back() {
        let graph = weekly().to_graph("2026-10-26", "Pacific Standard Time");
        assert_eq!(
            graph,
            json!({
                "pattern": { "type": "weekly", "interval": 2, "daysOfWeek": ["monday", "wednesday"], "firstDayOfWeek": "sunday" },
                "range": { "type": "numbered", "startDate": "2026-10-26", "numberOfOccurrences": 10, "recurrenceTimeZone": "Pacific Standard Time" }
            })
        );
        assert_eq!(Repeat::from_graph(&graph), Some(weekly()));

        let last_friday = Repeat {
            every: Every::Month,
            interval: 1,
            weekdays: vec![5],
            nth: Some(5),
            day: None,
            month: None,
            ends: Ends::On {
                date: "2027-06-30".into(),
            },
        };
        let graph = last_friday.to_graph("2026-10-30", "UTC");
        assert_eq!(graph["pattern"]["type"], "relativeMonthly");
        assert_eq!(graph["pattern"]["index"], "last");
        assert_eq!(graph["range"]["endDate"], "2027-06-30");
        assert_eq!(Repeat::from_graph(&graph), Some(last_friday));

        let birthday = Repeat {
            every: Every::Year,
            interval: 1,
            weekdays: vec![],
            nth: None,
            day: Some(27),
            month: Some(10),
            ends: Ends::Never,
        };
        let graph = birthday.to_graph("2026-10-27", "UTC");
        assert_eq!(graph["pattern"]["type"], "absoluteYearly");
        assert_eq!(Repeat::from_graph(&graph), Some(birthday));

        // Outlook's "no end" sometimes comes as an end date of year 1.
        let mut forever = weekly().to_graph("2026-10-26", "UTC");
        forever["range"] =
            json!({ "type": "endDate", "endDate": "0001-01-01", "startDate": "2026-10-26" });
        assert_eq!(Repeat::from_graph(&forever).unwrap().ends, Ends::Never);
        assert_eq!(
            Repeat::from_graph(&json!({ "pattern": { "type": "hourly" } })),
            None
        );
    }

    #[test]
    fn a_series_keeps_its_own_pattern_and_moves_with_its_day() {
        // Every other Saturday and Sunday, weeks starting on Monday.
        let old = json!({
            "pattern": { "type": "weekly", "interval": 2, "daysOfWeek": ["saturday", "sunday"], "firstDayOfWeek": "monday" },
            "range": { "type": "noEnd", "startDate": "2026-10-31", "recurrenceTimeZone": "W. Europe Standard Time" }
        });
        // Only its end changed: the pattern stays exactly as it was.
        let mut rule = Repeat::from_graph(&old).unwrap();
        rule.ends = Ends::After { times: 6 };
        let out = changed(&old, Some(&rule), 0, None);
        assert_eq!(out["pattern"], old["pattern"]);
        assert_eq!(
            out["range"],
            json!({ "type": "numbered", "startDate": "2026-10-31", "numberOfOccurrences": 6, "recurrenceTimeZone": "W. Europe Standard Time" })
        );
        // A new pattern keeps the week's first day.
        rule.weekdays = vec![6];
        assert_eq!(
            changed(&old, Some(&rule), 0, None)["pattern"]["firstDayOfWeek"],
            "monday"
        );

        // Dragged a day later: the days move with it, from its new first day.
        let tuesdays = Repeat {
            every: Every::Week,
            interval: 1,
            weekdays: vec![2],
            nth: None,
            day: None,
            month: None,
            ends: Ends::Never,
        }
        .to_graph("2026-11-10", "Pacific Standard Time");
        let out = changed(
            &tuesdays,
            None,
            1,
            Some(("2026-11-11", "Pacific Standard Time")),
        );
        assert_eq!(out["pattern"]["daysOfWeek"], json!(["wednesday"]));
        assert_eq!(out["range"]["startDate"], "2026-11-11");
        // Friday and Saturday every other week, a day later: Saturday and
        // Sunday, still in one week, as the week starts a day later too.
        let weekend = json!({
            "pattern": { "type": "weekly", "interval": 2, "daysOfWeek": ["friday", "saturday"], "firstDayOfWeek": "sunday" },
            "range": { "type": "noEnd", "startDate": "2026-10-30", "recurrenceTimeZone": "UTC" }
        });
        let out = changed(&weekend, None, 1, Some(("2026-10-31", "UTC")));
        assert_eq!(out["pattern"]["daysOfWeek"], json!(["saturday", "sunday"]));
        assert_eq!(out["pattern"]["firstDayOfWeek"], "monday");
        let monthly = json!({
            "pattern": { "type": "relativeMonthly", "interval": 1, "daysOfWeek": ["tuesday"], "index": "second" },
            "range": { "type": "noEnd", "startDate": "2026-10-13", "recurrenceTimeZone": "UTC" }
        });
        let out = changed(&monthly, None, 15, Some(("2026-10-28", "UTC")));
        assert_eq!(out["pattern"]["daysOfWeek"], json!(["wednesday"]));
        assert_eq!(out["pattern"]["index"], "last");
        assert_eq!(days_between("2026-10-27", "2026-11-03"), 7);
        assert_eq!(add_days("2026-12-31", 1), "2027-01-01");
        assert_eq!(
            ending_on(&tuesdays, "2027-01-26")["range"]["endDate"],
            "2027-01-26"
        );
    }

    #[test]
    fn a_series_ends_the_day_before_a_split() {
        let graph = weekly().to_graph("2026-10-26", "UTC");
        let ended = ended_before(&graph, "2026-11-01");
        assert_eq!(ended["range"]["type"], "endDate");
        assert_eq!(ended["range"]["endDate"], "2026-10-31");
        assert!(ended["range"].get("numberOfOccurrences").is_none());
        assert_eq!(ended["pattern"], graph["pattern"]);
        assert_eq!(day_before("2027-01-01"), "2026-12-31");
        assert_eq!(day_before("2028-03-01"), "2028-02-29");
    }
}
