//! Private, local counts-only usage history for the dashboard.
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Seek, SeekFrom, Write},
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::{Datelike, Local, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use tern_sdk::ui::ChartCol;

use crate::model::{Model, usage::UsageReport};

const MAX_READ_BYTES: u64 = 20 * 1024 * 1024;
const WEEKS: usize = 26;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    t: i64,
    agent: String,
    #[serde(rename = "in")]
    input: Option<u64>,
    cached: Option<u64>,
    #[serde(rename = "out")]
    output: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cost: Option<Cost>,
    ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Cost {
    currency: String,
    amount: f64,
}

#[derive(Clone, Debug, Default)]
pub struct Totals {
    pub tokens: u64,
    pub unknown_turns: u64,
    pub known_turns: u64,
    pub turns: u64,
    pub costs: BTreeMap<String, f64>,
}

impl Totals {
    fn add(&mut self, record: &Record) {
        self.turns += 1;
        match (record.input, record.output) {
            (Some(input), Some(output)) => {
                self.tokens = self.tokens.saturating_add(input.saturating_add(output));
                self.known_turns += 1;
            }
            _ => self.unknown_turns += 1,
        }
        if let Some(cost) = &record.cost
            && cost.amount.is_finite()
        {
            *self.costs.entry(cost.currency.clone()).or_default() += cost.amount;
        }
    }

    fn token_label(&self) -> String {
        if self.turns == 0 {
            "0 tokens".to_owned()
        } else if self.known_turns == 0 {
            "tokens unknown".to_owned()
        } else if self.unknown_turns > 0 {
            format!(
                "≥{} tokens ({} unknown)",
                compact(self.tokens),
                self.unknown_turns
            )
        } else {
            format!("{} tokens", compact(self.tokens))
        }
    }

    pub fn label(&self) -> String {
        let costs = self.costs.iter().map(|(currency, amount)| {
            if currency == "USD" {
                format!("${amount:.2}")
            } else {
                format!("{amount:.2} {currency}")
            }
        });
        std::iter::once(format!("{} turns · {}", self.turns, self.token_label()))
            .chain(costs)
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

fn compact(value: u64) -> String {
    if value >= 1_000_000 {
        format!("{:.1}M", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{:.1}K", value as f64 / 1_000.0)
    } else {
        value.to_string()
    }
}

#[derive(Clone, Debug)]
pub struct History {
    pub cells: Vec<Vec<Option<f64>>>,
    pub tips: Vec<Vec<Option<String>>>,
    pub cols: Vec<ChartCol>,
    pub today: Totals,
    pub week: Totals,
    pub month: Totals,
    pub turns: u64,
}

/// A Monday-first 26-week grid, plus the trailing 1, 7 and 30 local calendar days.
/// The timezone and clock are parameters so historical day boundaries are deterministic.
pub fn aggregate<Tz: TimeZone>(
    records: impl IntoIterator<Item = Record>,
    now: i64,
    tz: &Tz,
) -> History {
    let today = Utc
        .timestamp_opt(now, 0)
        .single()
        .expect("usage history clock must be a valid Unix timestamp")
        .with_timezone(tz)
        .date_naive();
    let start = today
        - chrono::Duration::days(
            i64::from(today.weekday().num_days_from_monday()) + 7 * (WEEKS as i64 - 1),
        );
    let mut days: BTreeMap<NaiveDate, Totals> = BTreeMap::new();
    let mut history = History {
        cells: vec![vec![None; WEEKS]; 7],
        tips: vec![vec![None; WEEKS]; 7],
        cols: Vec::new(),
        today: Totals::default(),
        week: Totals::default(),
        month: Totals::default(),
        turns: 0,
    };
    for record in records {
        let Some(date) = Utc
            .timestamp_opt(record.t, 0)
            .single()
            .map(|instant| instant.with_timezone(tz).date_naive())
        else {
            continue;
        };
        let offset = date.signed_duration_since(today).num_days();
        if offset > 0 {
            continue;
        }
        if offset == 0 {
            history.today.add(&record);
        }
        if offset >= -6 {
            history.week.add(&record);
        }
        if offset >= -29 {
            history.month.add(&record);
        }
        if date >= start {
            days.entry(date).or_default().add(&record);
            history.turns += 1;
        }
    }
    let maximum = days.values().map(|total| total.tokens).max().unwrap_or(0);
    let mut previous_month = None;
    for week in 0..WEEKS {
        let monday = start + chrono::Duration::weeks(week as i64);
        if previous_month != Some(monday.month()) {
            history.cols.push(ChartCol {
                at: week as u32,
                label: monday.format("%b").to_string(),
            });
            previous_month = Some(monday.month());
        }
        for day in 0..7 {
            let date = monday + chrono::Duration::days(day as i64);
            if date > today {
                continue;
            }
            let total = days.get(&date);
            history.cells[day][week] = Some(if maximum == 0 {
                0.0
            } else {
                total.map_or(0.0, |total| total.tokens as f64 / maximum as f64)
            });
            history.tips[day][week] = Some(format!(
                "{}: {}",
                date.format("%b %-d"),
                total.map_or_else(
                    || "0 tokens".to_owned(),
                    |total| format!(
                        "{} · {} turn{}",
                        total.token_label(),
                        total.turns,
                        if total.turns == 1 { "" } else { "s" }
                    ),
                )
            ));
        }
    }
    history
}

fn path() -> Option<PathBuf> {
    let root = std::env::var_os("XDG_STATE_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
        })?;
    Some(root.join("acp-tsp/usage.jsonl"))
}

fn read_at(path: &std::path::Path) -> io::Result<Vec<Record>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let len = file.metadata()?.len();
    if len > MAX_READ_BYTES {
        file.seek(SeekFrom::Start(len - MAX_READ_BYTES))?;
    }
    let mut reader = BufReader::new(file);
    if len > MAX_READ_BYTES {
        let mut partial = Vec::new();
        reader.read_until(b'\n', &mut partial)?;
    }
    let mut records = Vec::new();
    let mut line = Vec::new();
    while reader.read_until(b'\n', &mut line)? != 0 {
        if let Ok(record) = serde_json::from_slice::<Record>(&line) {
            records.push(record);
        }
        line.clear();
    }
    Ok(records)
}

pub fn refresh(report: &mut UsageReport) {
    let records = path().and_then(|path| match read_at(&path) {
        Ok(records) => Some(records),
        Err(error) => {
            tracing::debug!(%error, "could not read usage history");
            None
        }
    });
    report.history = records.map(|records| aggregate(records, Utc::now().timestamp(), &Local));
}

fn append(path: &std::path::Path, record: &Record) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "usage history has no parent")
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(path)?;
        if file.metadata()?.permissions().mode() & 0o777 != 0o600 {
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        write_record(&mut file, record)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(parent)?;
        let mut file = OpenOptions::new().append(true).create(true).open(path)?;
        write_record(&mut file, record)
    }
}

fn write_record(file: &mut File, record: &Record) -> io::Result<()> {
    let mut line = serde_json::to_vec(record)?;
    line.push(b'\n');
    // One O_APPEND write per turn: other instances cannot interleave JSON fragments.
    if file.write(&line)? != line.len() {
        return Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "partial usage history record",
        ));
    }
    Ok(())
}

pub struct Recorder {
    path: Option<PathBuf>,
    last_turns: u64,
    last_summary_id: u64,
    session_active: bool,
}

fn recording_enabled(value: Option<&std::ffi::OsStr>) -> bool {
    value != Some(std::ffi::OsStr::new("0"))
}

impl Recorder {
    pub fn new() -> Self {
        Self {
            path: recording_enabled(std::env::var_os("ACP_TSP_USAGE_HISTORY").as_deref())
                .then(path)
                .flatten(),
            last_turns: 0,
            last_summary_id: 0,
            session_active: false,
        }
    }

    /// Called after synchronizing each visible frame, not for each render operation.
    pub fn record(&mut self, model: &Model) {
        let Some(session) = &model.session else {
            self.session_active = false;
            self.last_turns = model.finished_turns;
            self.last_summary_id = 0;
            return;
        };
        if !self.session_active {
            self.session_active = true;
            // Loaded or forked sessions may replay prior turns before SessionStarted.
            if matches!(
                session.kind,
                crate::event::SessionStartKind::Load | crate::event::SessionStartKind::Fork
            ) {
                self.last_turns = model.finished_turns;
                self.last_summary_id = model.turn_summaries.keys().copied().max().unwrap_or(0);
                return;
            }
        }
        if model.finished_turns == self.last_turns || model.last_outcome.is_none() {
            return;
        }
        self.last_turns = model.finished_turns;
        let Some(path) = &self.path else {
            return;
        };
        let mut summaries: Vec<_> = model
            .turn_summaries
            .iter()
            .filter(|(id, _)| **id > self.last_summary_id)
            .collect();
        summaries.sort_unstable_by_key(|(id, _)| *id);
        self.last_summary_id = summaries
            .last()
            .map_or(self.last_summary_id, |(id, _)| **id);
        for (_, summary) in summaries {
            #[cfg(feature = "unstable_end_turn_token_usage")]
            let (input, cached, output) =
                summary.tokens.as_ref().map_or((None, None, None), |usage| {
                    let cache = usage
                        .cached_read_tokens
                        .unwrap_or(0)
                        .saturating_add(usage.cached_write_tokens.unwrap_or(0));
                    (
                        Some(usage.input_tokens.saturating_add(cache)),
                        Some(cache),
                        Some(usage.output_tokens),
                    )
                });
            #[cfg(not(feature = "unstable_end_turn_token_usage"))]
            let (input, cached, output) = (None, None, None);
            let record = Record {
                t: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64,
                agent: model
                    .agent
                    .as_ref()
                    .map_or_else(|| model.launch_name.clone(), |agent| agent.name.clone()),
                input,
                cached,
                output,
                cost: summary
                    .cost
                    .as_ref()
                    .filter(|(_, amount)| amount.is_finite())
                    .map(|(currency, amount)| Cost {
                        currency: currency.clone(),
                        amount: *amount,
                    }),
                ms: summary.duration.as_millis().min(u128::from(u64::MAX)) as u64,
            };
            if let Err(error) = append(path, &record) {
                tracing::debug!(%error, "could not append usage history");
            }
        }
    }
}

impl Default for Recorder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;
    use std::time::Duration;

    fn record(t: i64, input: Option<u64>, output: Option<u64>) -> Record {
        Record {
            t,
            agent: "agent".into(),
            input,
            output,
            cached: input.map(|_| 0),
            cost: None,
            ms: 100,
        }
    }

    #[test]
    fn history_opt_out_only_disables_recording() {
        assert!(!recording_enabled(Some(std::ffi::OsStr::new("0"))));
        assert!(recording_enabled(None));
        assert!(recording_enabled(Some(std::ffi::OsStr::new("1"))));
    }

    #[test]
    fn calendar_respects_local_midnight_and_window_edges() {
        let zone = FixedOffset::east_opt(2 * 3600).unwrap();
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-08T12:00:00+02:00")
            .unwrap()
            .timestamp();
        let start = chrono::DateTime::parse_from_rfc3339("2026-04-13T00:00:00+02:00")
            .unwrap()
            .timestamp();
        let mut data = [
            record(start - 1, Some(999), Some(1)),
            record(start, Some(2), Some(3)),
            record(now - 13 * 3600, Some(10), Some(2)),
            record(now + 5 * 3600, None, None),
        ];
        data[2].cost = Some(Cost {
            currency: "USD".into(),
            amount: 1.25,
        });
        data[3].cost = Some(Cost {
            currency: "EUR".into(),
            amount: 2.5,
        });
        let history = aggregate(data, now, &zone);
        assert_eq!(history.turns, 3);
        assert_eq!(history.cells.len(), 7);
        assert!(history.cells.iter().all(|row| row.len() == 26));
        assert_eq!(history.cells[0][0], Some(5.0 / 12.0));
        assert_eq!(history.today.turns, 1);
        assert_eq!(history.week.turns, 2);
        assert_eq!(history.month.turns, 2);
        assert_eq!(history.today.unknown_turns, 1);
        assert_eq!(history.today.known_turns, 0);
        assert_eq!(history.today.tokens, 0);
        assert_eq!(history.week.costs.get("USD"), Some(&1.25));
        assert_eq!(history.week.costs.get("EUR"), Some(&2.5));
        assert!(
            history.tips[3][25]
                .as_deref()
                .unwrap()
                .contains("tokens unknown")
        );
        assert!(history.tips[1][25].as_deref().unwrap().contains("0 tokens"));
        assert!(history.cells[4][25].is_none());
    }

    #[test]
    fn malformed_lines_are_ignored() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("usage.jsonl");
        fs::write(&path, b"invalid\n\xff\xfe\n{\"t\":1,\"agent\":\"a\",\"in\":null,\"cached\":null,\"out\":null,\"ms\":5}\n{broken\n").unwrap();
        let records = read_at(&path).unwrap();
        assert_eq!(records.len(), 1);
        assert!(records[0].input.is_none());
    }

    #[test]
    fn appends_one_line_per_live_turn_and_not_hidden_queries() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("acp-tsp/usage.jsonl");
        let mut recorder = Recorder {
            path: Some(path.clone()),
            last_turns: 0,
            last_summary_id: 0,
            session_active: false,
        };
        let mut model = Model::default();
        model.session = Some(crate::model::ActiveSession {
            id: agent_client_protocol::schema::v1::SessionId::new("test"),
            kind: crate::event::SessionStartKind::New,
            cwd: PathBuf::new(),
            additional_dirs: vec![],
            modes: None,
            config_options: None,
            title: None,
            updated_at: None,
        });
        model.turn_summaries.insert(
            1,
            crate::model::TurnSummary {
                duration: Duration::from_millis(100),
                #[cfg(feature = "unstable_end_turn_token_usage")]
                tokens: None,
                cost: None,
                stop: None,
            },
        );
        model.finished_turns = 1;
        model.last_outcome = Some(crate::model::TurnOutcome::Done);
        recorder.record(&model);
        let disabled_path = temp.path().join("disabled/usage.jsonl");
        let mut disabled = Recorder {
            path: recording_enabled(Some(std::ffi::OsStr::new("0")))
                .then_some(disabled_path.clone()),
            last_turns: 0,
            last_summary_id: 0,
            session_active: false,
        };
        disabled.record(&model);
        assert!(!disabled_path.exists());
        recorder.record(&model);
        model.begin_usage();
        model.apply(crate::event::BridgeEvent::TurnEnded(Err(
            "usage unavailable".into(),
        )));
        recorder.record(&model);
        assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 1);
        let parsed = read_at(&path).unwrap();
        assert_eq!(parsed[0].ms, 100);
        assert_eq!(parsed[0].input, None);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
    }

    #[test]
    fn replayed_load_turns_do_not_append() {
        let temp = tempfile::tempdir().unwrap();
        let mut recorder = Recorder {
            path: Some(temp.path().join("usage.jsonl")),
            last_turns: 0,
            last_summary_id: 0,
            session_active: false,
        };
        let mut model = Model::default();
        model.session = Some(crate::model::ActiveSession {
            id: agent_client_protocol::schema::v1::SessionId::new("test"),
            kind: crate::event::SessionStartKind::Load,
            cwd: PathBuf::new(),
            additional_dirs: vec![],
            modes: None,
            config_options: None,
            title: None,
            updated_at: None,
        });
        model.finished_turns = 2;
        recorder.record(&model);
        assert!(!recorder.path.as_ref().unwrap().exists());
    }
}
