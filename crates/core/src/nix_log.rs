//! Reading `nix --log-format internal-json`, so a build can be shown as counters instead of a
//! wall of text that scrolls past faster than anyone can read it.
//!
//! nix writes one JSON record per line, prefixed with `@nix `. Records describe activities that
//! start, report progress and stop, so the counts a user cares about (paths downloaded, bytes
//! fetched, derivations built) have to be accumulated as the stream arrives. What nix expects
//! grows and shrinks as it finds work, so totals come only from the plan it prints first.
//!
//! A build emits thousands of records a second, so every record is folded into the totals in
//! constant time: the aggregate per activity type is maintained incrementally instead of being
//! recomputed by walking the live activities.

use std::borrow::Cow;
use std::collections::HashMap;

use serde::de::{SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};

/// Prefix nix puts on every structured record it writes to stderr.
pub const RECORD_PREFIX: &str = "@nix ";

/// Activity types, from nix's `logging.hh`. Only the ones carrying a countable total are
/// tracked; the rest are dropped before they reach the activity map.
const ACT_COPY_PATH: u64 = 100;
const ACT_FILE_TRANSFER: u64 = 101;
const ACT_COPY_PATHS: u64 = 103;
const ACT_BUILDS: u64 = 104;
const ACT_BUILD: u64 = 105;

/// Result types, from nix's `logging.hh`.
const RES_PROGRESS: u64 = 105;

/// nix verbosity levels: anything past `info` is detail the user did not ask for.
const LVL_INFO: u8 = 3;

/// Slots in [`NixLog::totals`], one per tracked activity type.
const TRANSFER: usize = 0;
const DOWNLOADS: usize = 1;
const BUILDS: usize = 2;
const TRACKED_TYPES: usize = 3;

fn tracked(activity_type: u64) -> Option<usize> {
    match activity_type {
        ACT_FILE_TRANSFER => Some(TRANSFER),
        ACT_COPY_PATHS => Some(DOWNLOADS),
        ACT_BUILDS => Some(BUILDS),
        _ => None,
    }
}

/// What nix is doing right now, reduced to the handful of numbers worth putting on a line.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct BuildProgress {
    pub builds_done: u64,
    pub builds_expected: u64,
    pub builds_running: u64,
    pub downloads_done: u64,
    pub downloads_expected: u64,
    pub downloads_running: u64,
    pub bytes_done: u64,
    pub bytes_expected: u64,
}

impl BuildProgress {
    /// Whether there is nothing to draw yet: no counter has moved off zero.
    pub fn is_idle(&self) -> bool {
        *self == Self::default()
    }
}

/// What a record means to whoever is drawing the screen.
#[derive(Debug, PartialEq, Eq)]
pub enum Event<'a> {
    /// Not a structured record: an ordinary stderr line, to be shown as-is.
    Plain(&'a str),
    /// A diagnostic from nix: worth showing, and worth keeping to explain a failure with.
    Message(Cow<'a, str>),
    /// A description of what nix just started doing: worth showing while it lasts, not worth
    /// keeping.
    Transient(Cow<'a, str>),
    Building(&'a str),
    Fetching(&'a str),
    /// The counters moved; the caller should read [`NixLog::snapshot`].
    Progress,
    /// A record that changes nothing on screen.
    Ignored,
}

/// The running counts for one activity type.
///
/// `live_*` is the sum over the activities of this type that are still running, kept in step as
/// their progress is reported so a snapshot never has to walk them. Only what is done is counted
/// here: what nix expects grows and shrinks as it finds work, so totals come from its plan.
#[derive(Debug, Default, Clone, Copy)]
struct Totals {
    finished_done: u64,
    live_done: u64,
    live_running: u64,
}

impl Totals {
    fn done(&self) -> u64 {
        self.finished_done + self.live_done
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct Activity {
    slot: usize,
    done: u64,
    running: u64,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    pub builds: Option<u64>,
    pub fetches: Option<u64>,
    pub download_bytes: Option<u64>,
}

impl Plan {
    fn read(&mut self, message: &str) {
        if message == "this derivation will be built:" {
            self.builds = Some(1);
        } else if let Some(count) = message
            .strip_prefix("these ")
            .and_then(|rest| rest.strip_suffix(" derivations will be built:"))
        {
            self.builds = count.parse().ok();
        } else if let Some(sizes) = message.strip_prefix("this path will be fetched (") {
            self.fetches = Some(1);
            self.read_sizes(sizes);
        } else if let Some((count, sizes)) = message
            .strip_prefix("these ")
            .and_then(|rest| rest.split_once(" paths will be fetched ("))
        {
            self.fetches = count.parse().ok();
            self.read_sizes(sizes);
        }
    }

    fn read_sizes(&mut self, sizes: &str) {
        if let Some((download, _)) = sizes.split_once(" download, ") {
            self.download_bytes = size(download);
        }
    }
}

fn size(rendered: &str) -> Option<u64> {
    let (number, unit) = rendered.split_once(' ')?;
    let power = match unit {
        "KiB" => 1,
        "MiB" => 2,
        "GiB" => 3,
        "TiB" => 4,
        "PiB" => 5,
        "EiB" => 6,
        _ => return None,
    };
    let value: f64 = number.parse().ok()?;
    Some((value * 1024f64.powi(power)) as u64)
}

/// Folds a stream of `internal-json` records into [`BuildProgress`].
#[derive(Debug, Default)]
pub struct NixLog {
    live: HashMap<u64, Activity>,
    totals: [Totals; TRACKED_TYPES],
    structured: bool,
    plan: Plan,
}

impl NixLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether any structured record has been seen, i.e. whether the counters mean anything.
    pub fn is_structured(&self) -> bool {
        self.structured
    }

    pub fn plan(&self) -> Plan {
        self.plan
    }

    pub fn snapshot(&self) -> BuildProgress {
        let builds = &self.totals[BUILDS];
        let downloads = &self.totals[DOWNLOADS];
        let bytes = &self.totals[TRANSFER];
        let total = |planned: Option<u64>, totals: &Totals| {
            planned.map_or(0, |planned| planned.max(totals.done()))
        };

        BuildProgress {
            builds_done: builds.done(),
            builds_expected: total(self.plan.builds, builds),
            builds_running: builds.live_running,
            downloads_done: downloads.done(),
            downloads_expected: total(self.plan.fetches, downloads),
            downloads_running: downloads.live_running,
            bytes_done: bytes.done(),
            bytes_expected: total(self.plan.download_bytes, bytes),
        }
    }

    /// Folds one stderr line into the totals.
    ///
    /// A line that is not a record, or a record that does not parse, is handed back untouched so
    /// a plain-text stream flows through unchanged.
    #[inline]
    pub fn observe<'a>(&mut self, line: &'a str) -> Event<'a> {
        match line.strip_prefix(RECORD_PREFIX) {
            Some(payload) => self.observe_record(line, payload),
            None => Event::Plain(line),
        }
    }

    /// Kept out of line so the plain-line check above stays small enough to inline.
    #[inline(never)]
    fn observe_record<'a>(&mut self, line: &'a str, payload: &'a str) -> Event<'a> {
        let Ok(record) = serde_json::from_str::<Record>(payload) else {
            return Event::Plain(line);
        };
        self.structured = true;

        match record.action {
            "start" => self.start(record),
            "stop" => self.stop(record.id),
            "result" => self.result(record),
            "msg" if record.level <= LVL_INFO && !record.msg.is_empty() => {
                self.plan.read(&record.msg);
                Event::Message(record.msg)
            }
            _ => Event::Ignored,
        }
    }

    fn start<'a>(&mut self, record: Record<'a>) -> Event<'a> {
        if record.activity_type == ACT_BUILD
            && let Some(derivation) = record.fields.text
        {
            return Event::Building(derivation);
        }

        if record.activity_type == ACT_COPY_PATH {
            return match record.fields.text {
                Some(path) => Event::Fetching(path),
                None => Event::Ignored,
            };
        }

        let Some(slot) = tracked(record.activity_type) else {
            if record.level <= LVL_INFO && !record.text.is_empty() {
                return Event::Transient(record.text);
            }
            return Event::Ignored;
        };

        self.live.insert(
            record.id,
            Activity {
                slot,
                ..Activity::default()
            },
        );
        Event::Progress
    }

    fn stop(&mut self, id: u64) -> Event<'static> {
        let Some(activity) = self.live.remove(&id) else {
            return Event::Ignored;
        };
        let totals = &mut self.totals[activity.slot];
        totals.finished_done += activity.done;
        totals.live_done = totals.live_done.saturating_sub(activity.done);
        totals.live_running = totals.live_running.saturating_sub(activity.running);
        Event::Progress
    }

    fn result<'a>(&mut self, record: Record<'a>) -> Event<'a> {
        match record.activity_type {
            RES_PROGRESS => self.report_progress(record.id, &record.fields),
            _ => Event::Ignored,
        }
    }

    fn report_progress(&mut self, id: u64, fields: &Fields) -> Event<'static> {
        let Some(activity) = self.live.get_mut(&id) else {
            return Event::Ignored;
        };
        let (done, running) = (fields.int(0), fields.int(2));

        let totals = &mut self.totals[activity.slot];
        totals.live_done = totals.live_done.saturating_sub(activity.done) + done;
        totals.live_running = totals.live_running.saturating_sub(activity.running) + running;

        activity.done = done;
        activity.running = running;
        Event::Progress
    }
}

/// One `internal-json` record.
///
/// Borrows from the line it was parsed out of, so a record that carries no text costs no
/// allocation at all.
#[derive(Deserialize)]
struct Record<'a> {
    action: &'a str,
    #[serde(default)]
    id: u64,
    #[serde(default)]
    level: u8,
    #[serde(default, rename = "type")]
    activity_type: u64,
    #[serde(default, borrow)]
    text: Cow<'a, str>,
    #[serde(default, borrow)]
    msg: Cow<'a, str>,
    #[serde(default, borrow)]
    fields: Fields<'a>,
}

/// The leading integers of a record's `fields` array.
///
/// Every field the counters need is an integer in the first few positions, so they are read into
/// a fixed array: no record ever allocates a vector, whatever nix decides to put in there.
const MAX_FIELDS: usize = 4;

#[derive(Debug, Default)]
struct Fields<'a> {
    ints: [u64; MAX_FIELDS],
    text: Option<&'a str>,
}

impl Fields<'_> {
    fn int(&self, index: usize) -> u64 {
        self.ints.get(index).copied().unwrap_or(0)
    }
}

impl<'de: 'a, 'a> Deserialize<'de> for Fields<'a> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct FieldsVisitor<'a>(std::marker::PhantomData<&'a ()>);

        impl<'de: 'a, 'a> Visitor<'de> for FieldsVisitor<'a> {
            type Value = Fields<'a>;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an array of activity fields")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Fields<'a>, A::Error> {
                let mut fields = Fields::default();
                let mut index = 0;
                while let Some(field) = seq.next_element::<Field<'de>>()? {
                    match field {
                        Field::Int(value) if index < MAX_FIELDS => fields.ints[index] = value,
                        Field::Text(text) if index == 0 => fields.text = Some(text),
                        Field::Escaped if index == 0 => fields.text = Some(""),
                        _ => {}
                    }
                    index += 1;
                }
                Ok(fields)
            }
        }

        deserializer.deserialize_seq(FieldsVisitor(std::marker::PhantomData))
    }
}

enum Field<'a> {
    Int(u64),
    Text(&'a str),
    Escaped,
    Other,
}

impl<'de: 'a, 'a> Deserialize<'de> for Field<'a> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct FieldVisitor<'a>(std::marker::PhantomData<&'a ()>);

        impl<'de: 'a, 'a> Visitor<'de> for FieldVisitor<'a> {
            type Value = Field<'a>;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an activity field")
            }

            fn visit_u64<E>(self, value: u64) -> Result<Field<'a>, E> {
                Ok(Field::Int(value))
            }

            fn visit_i64<E>(self, value: i64) -> Result<Field<'a>, E> {
                Ok(Field::Int(value.max(0) as u64))
            }

            fn visit_f64<E>(self, value: f64) -> Result<Field<'a>, E> {
                Ok(Field::Int(value.max(0.0) as u64))
            }

            fn visit_bool<E>(self, _value: bool) -> Result<Field<'a>, E> {
                Ok(Field::Other)
            }

            fn visit_borrowed_str<E>(self, value: &'de str) -> Result<Field<'a>, E> {
                Ok(Field::Text(value))
            }

            fn visit_str<E>(self, _value: &str) -> Result<Field<'a>, E> {
                Ok(Field::Escaped)
            }

            fn visit_unit<E>(self) -> Result<Field<'a>, E> {
                Ok(Field::Other)
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Field<'a>, A::Error> {
                while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
                Ok(Field::Other)
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Field<'a>, A::Error> {
                while map
                    .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
                    .is_some()
                {}
                Ok(Field::Other)
            }
        }

        deserializer.deserialize_any(FieldVisitor(std::marker::PhantomData))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NixFailure {
    Build { package: String },
    UnknownPackage { name: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NixError {
    pub message: String,
    pub failure: Option<NixFailure>,
}

fn without_escapes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        if chars.next() == Some('[') {
            for c in chars.by_ref() {
                if matches!(c, '@'..='~') {
                    break;
                }
            }
        }
    }
    out
}

fn quoted_after<'a>(text: &'a str, opening: &str) -> Option<&'a str> {
    let start = text.find(opening)? + opening.len();
    let length = text[start..].find('\'')?;
    Some(&text[start..start + length])
}

pub fn package_name(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    let name = name.strip_suffix(".drv").unwrap_or(name);
    match name.split_once('-') {
        Some((hash, rest)) if hash.len() == 32 => rest,
        _ => name,
    }
}

fn package_of(derivation: &str) -> String {
    package_name(derivation).to_string()
}

pub fn nix_error(tail: &str) -> Option<NixError> {
    let clean = without_escapes(tail);
    let start = clean
        .match_indices("error:")
        .map(|(at, _)| at)
        .filter(|at| *at == 0 || clean[..*at].ends_with(char::is_whitespace))
        .last()?;
    let message = clean[start..].trim_end().to_string();
    let failure = if let Some(derivation) = quoted_after(&message, "Cannot build '") {
        Some(NixFailure::Build {
            package: package_of(derivation),
        })
    } else {
        quoted_after(&message, "attribute '")
            .filter(|_| message.contains("' missing"))
            .map(|name| NixFailure::UnknownPackage {
                name: name.to_string(),
            })
    };
    Some(NixError { message, failure })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(id: u64, activity_type: u64) -> String {
        format!(
            r#"@nix {{"action":"start","id":{id},"level":3,"parent":0,"text":"","type":{activity_type},"fields":[]}}"#
        )
    }

    fn message(text: &str) -> String {
        format!(
            r#"@nix {{"action":"msg","level":3,"msg":{}}}"#,
            serde_json::json!(text)
        )
    }

    #[test]
    fn nix_s_plan_fixes_the_totals_while_its_own_rise_and_fall() {
        let mut log = NixLog::new();
        log.observe(&message(
            "these 3 paths will be fetched (354.1 KiB download, 3.2 MiB unpacked):",
        ));
        log.observe(&message(
            "  /nix/store/xl1h9i29pgq2q5cszjhm5wpfxfbbqwyi-hello-2.12.3",
        ));
        log.observe(&message("these 2 derivations will be built:"));
        log.observe(&start(1, ACT_COPY_PATHS));
        log.observe(&start(2, ACT_BUILDS));

        for (done, expected) in [(0, 1), (1, 5), (2, 2)] {
            log.observe(&progress(1, done, expected, 0));
            log.observe(&progress(2, done.min(1), expected + 7, 0));
            let snapshot = log.snapshot();
            assert_eq!(snapshot.downloads_expected, 3);
            assert_eq!(snapshot.builds_expected, 2);
        }
        assert_eq!(
            log.plan(),
            Plan {
                builds: Some(2),
                fetches: Some(3),
                download_bytes: Some((354.1f64 * 1024.0) as u64),
            }
        );
    }

    #[test]
    fn a_plan_of_one_reads_as_one_and_work_beyond_it_is_never_hidden() {
        let mut log = NixLog::new();
        log.observe(&message(
            "this path will be fetched (1.0 MiB download, 4.0 MiB unpacked):",
        ));
        log.observe(&message("this derivation will be built:"));
        log.observe(&start(1, ACT_COPY_PATHS));
        log.observe(&progress(1, 2, 2, 0));

        let snapshot = log.snapshot();
        assert_eq!(log.plan().builds, Some(1));
        assert_eq!(snapshot.downloads_done, 2);
        assert_eq!(snapshot.downloads_expected, 2);
    }

    #[test]
    fn a_copied_path_is_reported_by_its_store_path() {
        let mut log = NixLog::new();
        let line = r#"@nix {"action":"start","id":4,"level":3,"parent":0,"text":"copying path","type":100,"fields":["/nix/store/xl1h9i29pgq2q5cszjhm5wpfxfbbqwyi-hello-2.12.3","https://cache","local"]}"#;
        assert_eq!(
            log.observe(line),
            Event::Fetching("/nix/store/xl1h9i29pgq2q5cszjhm5wpfxfbbqwyi-hello-2.12.3")
        );
        assert_eq!(
            package_name("/nix/store/xl1h9i29pgq2q5cszjhm5wpfxfbbqwyi-hello-2.12.3"),
            "hello-2.12.3"
        );
    }

    fn progress(id: u64, done: u64, expected: u64, running: u64) -> String {
        format!(
            r#"@nix {{"action":"result","id":{id},"type":105,"fields":[{done},{expected},{running},0]}}"#
        )
    }

    fn stop(id: u64) -> String {
        format!(r#"@nix {{"action":"stop","id":{id}}}"#)
    }

    #[test]
    fn a_plain_line_is_handed_back_untouched() {
        let mut log = NixLog::new();
        assert_eq!(log.observe("copying path"), Event::Plain("copying path"));
        assert!(!log.is_structured());
    }

    #[test]
    fn a_record_that_does_not_parse_is_treated_as_a_plain_line() {
        let mut log = NixLog::new();
        let line = "@nix not json at all";
        assert_eq!(log.observe(line), Event::Plain(line));
        assert!(!log.is_structured());
    }

    #[test]
    fn a_message_is_reported_for_display_and_for_the_error() {
        let mut log = NixLog::new();
        let event = log.observe(r#"@nix {"action":"msg","level":0,"msg":"error: oh no"}"#);
        assert_eq!(event, Event::Message(Cow::Borrowed("error: oh no")));
        assert!(log.is_structured());
    }

    #[test]
    fn a_message_below_the_verbosity_budget_is_dropped() {
        let mut log = NixLog::new();
        let event = log.observe(r#"@nix {"action":"msg","level":6,"msg":"chatty detail"}"#);
        assert_eq!(event, Event::Ignored);
    }

    #[test]
    fn an_escaped_message_is_still_decoded() {
        let mut log = NixLog::new();
        let event = log.observe(r#"@nix {"action":"msg","level":0,"msg":"error: \"quoted\""}"#);
        assert_eq!(
            event,
            Event::Message(Cow::Owned(r#"error: "quoted""#.into()))
        );
    }

    #[test]
    fn an_untracked_activity_reports_its_text() {
        let mut log = NixLog::new();
        let event = log.observe(
            r#"@nix {"action":"start","id":1,"level":3,"text":"building '/nix/store/x.drv'","type":105,"fields":[]}"#,
        );
        assert_eq!(
            event,
            Event::Transient(Cow::Borrowed("building '/nix/store/x.drv'"))
        );
        assert!(log.snapshot().is_idle());
    }

    #[test]
    fn a_build_starting_names_the_derivation_being_built() {
        let mut log = NixLog::new();
        let event = log.observe(
            r#"@nix {"action":"start","fields":["/nix/store/x-a.drv","",1,1],"id":1,"level":3,"parent":0,"text":"building '/nix/store/x-a.drv'","type":105}"#,
        );

        assert_eq!(event, Event::Building("/nix/store/x-a.drv"));
    }

    #[test]
    fn a_build_is_reported_whatever_its_verbosity() {
        let mut log = NixLog::new();
        let event = log.observe(
            r#"@nix {"action":"start","fields":["/nix/store/x-a.drv"],"id":1,"level":7,"text":"","type":105}"#,
        );

        assert!(matches!(event, Event::Building(_)));
    }

    #[test]
    fn progress_on_a_build_is_not_mistaken_for_a_build_starting() {
        let mut log = NixLog::new();
        let event = log.observe(
            r#"@nix {"action":"result","fields":["/nix/store/x-a.drv"],"id":1,"type":105}"#,
        );

        assert!(!matches!(event, Event::Building(_)));
    }

    fn decoded(log: &str) -> String {
        let mut parser = NixLog::new();
        let mut tail = String::new();
        for line in log.lines() {
            if let Event::Message(message) = parser.observe(line) {
                tail.push_str(&message);
                tail.push('\n');
            }
        }
        tail
    }

    #[test]
    fn nixs_own_logs_are_read_as_the_failures_they_are() {
        let build = nix_error(&decoded(include_str!("../fixtures/nix/build-log.txt"))).unwrap();
        let missing = nix_error(&decoded(include_str!("../fixtures/nix/missing-log.txt"))).unwrap();

        assert_eq!(
            build.failure,
            Some(NixFailure::Build {
                package: "failing-5.0".into()
            })
        );
        assert_eq!(
            missing.failure,
            Some(NixFailure::UnknownPackage {
                name: "ripgrep2".into()
            })
        );
        assert!(
            missing
                .message
                .starts_with("error: attribute 'ripgrep2' missing")
        );
    }

    #[test]
    fn every_build_in_a_real_log_is_reported_once() {
        let mut log = NixLog::new();
        let builds: Vec<String> = include_str!("../fixtures/nix/build-log.txt")
            .lines()
            .filter_map(|line| match log.observe(line) {
                Event::Building(derivation) => Some(derivation.to_string()),
                _ => None,
            })
            .collect();

        assert_eq!(builds.len(), 1);
        assert!(builds[0].starts_with("/nix/store/"));
        assert!(builds[0].ends_with("-failing-5.0.drv"));
    }

    #[test]
    fn a_build_whose_derivation_cannot_be_borrowed_is_still_reported() {
        let mut log = NixLog::new();
        let event = log.observe(
            r#"@nix {"action":"start","fields":["/nix/store/x\u002da.drv"],"id":1,"level":3,"text":"","type":105}"#,
        );

        assert_eq!(event, Event::Building(""));
    }

    #[test]
    fn an_event_is_no_larger_than_a_borrowed_message() {
        assert_eq!(
            std::mem::size_of::<Event>(),
            std::mem::size_of::<Cow<str>>() + std::mem::size_of::<usize>()
        );
    }

    #[test]
    fn nothing_is_drawn_before_the_first_counter_moves() {
        assert!(NixLog::new().snapshot().is_idle());
    }

    #[test]
    fn build_counts_are_accumulated_against_the_plan() {
        let mut log = NixLog::new();
        log.observe(&message("these 17 derivations will be built:"));
        log.observe(&start(1, ACT_BUILDS));
        log.observe(&progress(1, 3, 40, 2));

        let snapshot = log.snapshot();
        assert_eq!(snapshot.builds_done, 3);
        assert_eq!(snapshot.builds_expected, 17);
        assert_eq!(snapshot.builds_running, 2);
        assert!(!snapshot.is_idle());
    }

    #[test]
    fn without_a_plan_nothing_is_expected_whatever_nix_estimates() {
        let mut log = NixLog::new();
        log.observe(&start(1, ACT_BUILDS));
        log.observe(&progress(1, 0, 1, 0));

        let snapshot = log.snapshot();
        assert_eq!(snapshot.builds_expected, 0);
        assert_eq!(snapshot.downloads_expected, 0);
        assert_eq!(snapshot.bytes_expected, 0);
    }

    #[test]
    fn downloads_and_their_bytes_are_counted_separately() {
        let mut log = NixLog::new();
        log.observe(&message(
            "these 37 paths will be fetched (4.4 KiB download, 9.0 KiB unpacked):",
        ));
        log.observe(&start(1, ACT_COPY_PATHS));
        log.observe(&progress(1, 12, 99, 1));
        log.observe(&start(2, ACT_FILE_TRANSFER));
        log.observe(&progress(2, 1_000, 4_000, 0));
        log.observe(&start(3, ACT_FILE_TRANSFER));
        log.observe(&progress(3, 500, 500, 0));

        let snapshot = log.snapshot();
        assert_eq!(
            (snapshot.downloads_done, snapshot.downloads_expected),
            (12, 37)
        );
        assert_eq!(
            (snapshot.bytes_done, snapshot.bytes_expected),
            (1_500, (4.4f64 * 1024.0) as u64)
        );
    }

    #[test]
    fn a_finished_activity_keeps_its_work_in_the_totals() {
        let mut log = NixLog::new();
        log.observe(&start(1, ACT_FILE_TRANSFER));
        log.observe(&progress(1, 2_048, 2_048, 0));
        log.observe(&stop(1));
        log.observe(&start(2, ACT_FILE_TRANSFER));
        log.observe(&progress(2, 512, 4_096, 0));

        assert_eq!(log.snapshot().bytes_done, 2_560);
        assert!(log.live.len() == 1, "the stopped activity should be gone");
    }

    #[test]
    fn progress_for_an_unknown_activity_is_ignored() {
        let mut log = NixLog::new();
        assert_eq!(log.observe(&progress(404, 1, 2, 0)), Event::Ignored);
        assert!(log.snapshot().is_idle());
    }

    #[test]
    fn an_untracked_activity_is_not_kept_in_the_map() {
        let mut log = NixLog::new();
        log.observe(&start(1, 109));
        log.observe(&stop(1));
        assert!(log.live.is_empty());
    }

    #[test]
    fn string_fields_do_not_disturb_the_counters() {
        let mut log = NixLog::new();
        log.observe(
            r#"@nix {"action":"start","id":1,"level":3,"text":"","type":101,"fields":["https://cache.nixos.org/nar/x","local"]}"#,
        );
        log.observe(&progress(1, 10, 20, 0));
        assert_eq!(log.snapshot().bytes_done, 10);
    }

    #[test]
    fn a_field_array_longer_than_the_budget_is_drained_without_failing() {
        let mut log = NixLog::new();
        log.observe(&start(1, ACT_BUILDS));
        let event = log.observe(
            r#"@nix {"action":"result","id":1,"type":105,"fields":[1,2,3,4,5,6,{"a":1},[7,8]]}"#,
        );
        assert_eq!(event, Event::Progress);
        assert_eq!(log.snapshot().builds_done, 1);
    }

    #[test]
    fn the_whole_stream_of_a_small_build_lands_on_sensible_totals() {
        let mut log = NixLog::new();
        for line in [
            start(1, ACT_COPY_PATHS),
            start(2, ACT_BUILDS),
            start(3, ACT_FILE_TRANSFER),
            progress(3, 2_048, 4_096, 0),
            progress(1, 0, 2, 1),
            stop(3),
            progress(1, 1, 2, 0),
            progress(2, 1, 1, 0),
            stop(2),
            stop(1),
        ] {
            log.observe(&line);
        }

        let snapshot = log.snapshot();
        assert_eq!(snapshot.builds_done, 1);
        assert_eq!(snapshot.downloads_done, 1);
        assert_eq!(snapshot.bytes_done, 2_048);
        assert!(log.live.is_empty(), "every activity stopped");
    }

    #[test]
    fn a_failed_build_is_named_by_its_package_and_keeps_nixs_words() {
        let tail = "this derivation will be built:\n  /nix/store/x.drv\n\u{1b}[31;1merror:\u{1b}[0m Cannot build '\u{1b}[35;1m/nix/store/bbx79xgf89bvd25i1sivdcykhy39bz14-failing-5.0.drv\u{1b}[0m'.\n       Reason: \u{1b}[31;1mbuilder failed with exit code 1\u{1b}[0m.";

        let error = nix_error(tail).unwrap();

        assert_eq!(
            error.failure,
            Some(NixFailure::Build {
                package: "failing-5.0".into()
            })
        );
        assert!(
            error
                .message
                .starts_with("error: Cannot build '/nix/store/")
        );
        assert!(
            error
                .message
                .ends_with("Reason: builder failed with exit code 1.")
        );
    }

    #[test]
    fn a_missing_attribute_is_an_unknown_package() {
        let tail = "\u{1b}[31;1merror:\u{1b}[0m attribute '\u{1b}[35;1mripgrep2\u{1b}[0m' missing\n       at /tmp/plan.nix:9:47:";

        assert_eq!(
            nix_error(tail).unwrap().failure,
            Some(NixFailure::UnknownPackage {
                name: "ripgrep2".into()
            })
        );
    }

    #[test]
    fn an_error_nix_does_not_classify_keeps_its_words_and_no_output_is_no_error() {
        let error =
            nix_error("warning: slow\nerror: getting status of '/x': No such file").unwrap();

        assert_eq!(error.failure, None);
        assert_eq!(error.message, "error: getting status of '/x': No such file");
        assert_eq!(nix_error("fatal: not a git repository"), None);
    }
}
