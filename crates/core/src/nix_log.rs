//! Reading `nix --log-format internal-json`, so a build can be shown as counters instead of a
//! wall of text that scrolls past faster than anyone can read it.
//!
//! nix writes one JSON record per line, prefixed with `@nix `. Records describe activities that
//! start, report progress and stop, so the totals a user cares about — paths downloaded, bytes
//! fetched, derivations built — have to be accumulated as the stream arrives.
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

/// Result types, from nix's `logging.hh`.
const RES_PROGRESS: u64 = 105;
const RES_SET_EXPECTED: u64 = 106;

/// nix verbosity levels: anything past `info` is detail the user did not ask for.
const LVL_INFO: u8 = 3;

/// Slots in [`NixLog::totals`], one per tracked activity type.
const BYTES: usize = 0;
const TRANSFER: usize = 1;
const DOWNLOADS: usize = 2;
const BUILDS: usize = 3;
const TRACKED_TYPES: usize = 4;

fn tracked(activity_type: u64) -> Option<usize> {
    match activity_type {
        ACT_COPY_PATH => Some(BYTES),
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
    /// The counters moved; the caller should read [`NixLog::snapshot`].
    Progress,
    /// A record that changes nothing on screen.
    Ignored,
}

/// The running totals for one activity type.
///
/// `live_*` is the sum over the activities of this type that are still running, kept in step as
/// their progress is reported so a snapshot never has to walk them.
#[derive(Debug, Default, Clone, Copy)]
struct Totals {
    finished_done: u64,
    live_done: u64,
    live_expected: u64,
    live_running: u64,
    /// Lower bound published by a parent activity via `resSetExpected`, e.g. the download size
    /// the whole build is going to need, which is known before any of it has been fetched.
    expected_floor: u64,
    floor_owner: Option<u64>,
}

impl Totals {
    fn done(&self) -> u64 {
        self.finished_done + self.live_done
    }

    /// Work already finished counts towards the total, the same way nix's own progress bar
    /// counts it: an activity that has stopped no longer publishes an expectation.
    fn expected(&self) -> u64 {
        (self.finished_done + self.live_expected).max(self.expected_floor)
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct Activity {
    slot: usize,
    done: u64,
    expected: u64,
    running: u64,
}

/// Folds a stream of `internal-json` records into [`BuildProgress`].
#[derive(Debug, Default)]
pub struct NixLog {
    live: HashMap<u64, Activity>,
    totals: [Totals; TRACKED_TYPES],
    structured: bool,
}

impl NixLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether any structured record has been seen, i.e. whether the counters mean anything.
    pub fn is_structured(&self) -> bool {
        self.structured
    }

    pub fn snapshot(&self) -> BuildProgress {
        let builds = &self.totals[BUILDS];
        let downloads = &self.totals[DOWNLOADS];

        // Bytes are reported per copied path; before a substituter is picked, the only byte
        // counts nix knows about are the raw file transfers.
        let mut bytes = &self.totals[BYTES];
        if bytes.done() == 0 && bytes.expected() == 0 {
            bytes = &self.totals[TRANSFER];
        }

        BuildProgress {
            builds_done: builds.done(),
            builds_expected: builds.expected(),
            builds_running: builds.live_running,
            downloads_done: downloads.done(),
            downloads_expected: downloads.expected(),
            downloads_running: downloads.live_running,
            bytes_done: bytes.done(),
            bytes_expected: bytes.expected(),
        }
    }

    /// Folds one stderr line into the totals.
    ///
    /// A line that is not a record, or a record that does not parse, is handed back untouched so
    /// a plain-text stream flows through unchanged.
    pub fn observe<'a>(&mut self, line: &'a str) -> Event<'a> {
        let Some(payload) = line.strip_prefix(RECORD_PREFIX) else {
            return Event::Plain(line);
        };
        let Ok(record) = serde_json::from_str::<Record>(payload) else {
            return Event::Plain(line);
        };
        self.structured = true;

        match record.action {
            "start" => self.start(record),
            "stop" => self.stop(record.id),
            "result" => self.result(record),
            "msg" if record.level <= LVL_INFO && !record.msg.is_empty() => {
                Event::Message(record.msg)
            }
            _ => Event::Ignored,
        }
    }

    fn start<'a>(&mut self, record: Record<'a>) -> Event<'a> {
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
        for totals in &mut self.totals {
            if totals.floor_owner == Some(id) {
                totals.expected_floor = 0;
                totals.floor_owner = None;
            }
        }

        let Some(activity) = self.live.remove(&id) else {
            return Event::Ignored;
        };
        let totals = &mut self.totals[activity.slot];
        totals.finished_done += activity.done;
        totals.live_done = totals.live_done.saturating_sub(activity.done);
        totals.live_expected = totals.live_expected.saturating_sub(activity.expected);
        totals.live_running = totals.live_running.saturating_sub(activity.running);
        Event::Progress
    }

    fn result<'a>(&mut self, record: Record<'a>) -> Event<'a> {
        match record.activity_type {
            RES_PROGRESS => self.report_progress(record.id, &record.fields),
            RES_SET_EXPECTED => self.set_expected(record.id, &record.fields),
            _ => Event::Ignored,
        }
    }

    fn report_progress(&mut self, id: u64, fields: &Fields) -> Event<'static> {
        let Some(activity) = self.live.get_mut(&id) else {
            return Event::Ignored;
        };
        let (done, expected, running) = (fields.int(0), fields.int(1), fields.int(2));

        let totals = &mut self.totals[activity.slot];
        totals.live_done = totals.live_done.saturating_sub(activity.done) + done;
        totals.live_expected = totals.live_expected.saturating_sub(activity.expected) + expected;
        totals.live_running = totals.live_running.saturating_sub(activity.running) + running;

        activity.done = done;
        activity.expected = expected;
        activity.running = running;
        Event::Progress
    }

    fn set_expected(&mut self, id: u64, fields: &Fields) -> Event<'static> {
        let Some(slot) = tracked(fields.int(0)) else {
            return Event::Ignored;
        };
        let totals = &mut self.totals[slot];
        totals.expected_floor = fields.int(1);
        totals.floor_owner = Some(id);
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
    #[serde(default)]
    fields: Fields,
}

/// The leading integers of a record's `fields` array.
///
/// Every field the counters need is an integer in the first few positions, so they are read into
/// a fixed array: no record ever allocates a vector, whatever nix decides to put in there.
const MAX_FIELDS: usize = 4;

#[derive(Debug, Default, Clone, Copy)]
struct Fields {
    ints: [u64; MAX_FIELDS],
}

impl Fields {
    fn int(&self, index: usize) -> u64 {
        self.ints.get(index).copied().unwrap_or(0)
    }
}

impl<'de> Deserialize<'de> for Fields {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct FieldsVisitor;

        impl<'de> Visitor<'de> for FieldsVisitor {
            type Value = Fields;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an array of activity fields")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Fields, A::Error> {
                let mut fields = Fields::default();
                let mut index = 0;
                while let Some(field) = seq.next_element::<MaybeInt>()? {
                    if index < MAX_FIELDS {
                        fields.ints[index] = field.0;
                    }
                    index += 1;
                }
                Ok(fields)
            }
        }

        deserializer.deserialize_seq(FieldsVisitor)
    }
}

/// A field read as an integer, or zero for the string fields the counters do not use.
struct MaybeInt(u64);

impl<'de> Deserialize<'de> for MaybeInt {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct MaybeIntVisitor;

        impl<'de> Visitor<'de> for MaybeIntVisitor {
            type Value = MaybeInt;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an activity field")
            }

            fn visit_u64<E>(self, value: u64) -> Result<MaybeInt, E> {
                Ok(MaybeInt(value))
            }

            fn visit_i64<E>(self, value: i64) -> Result<MaybeInt, E> {
                Ok(MaybeInt(value.max(0) as u64))
            }

            fn visit_f64<E>(self, value: f64) -> Result<MaybeInt, E> {
                Ok(MaybeInt(value.max(0.0) as u64))
            }

            fn visit_bool<E>(self, _value: bool) -> Result<MaybeInt, E> {
                Ok(MaybeInt(0))
            }

            fn visit_str<E>(self, _value: &str) -> Result<MaybeInt, E> {
                Ok(MaybeInt(0))
            }

            fn visit_unit<E>(self) -> Result<MaybeInt, E> {
                Ok(MaybeInt(0))
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<MaybeInt, A::Error> {
                while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
                Ok(MaybeInt(0))
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<MaybeInt, A::Error> {
                while map
                    .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
                    .is_some()
                {}
                Ok(MaybeInt(0))
            }
        }

        deserializer.deserialize_any(MaybeIntVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(id: u64, activity_type: u64) -> String {
        format!(
            r#"@nix {{"action":"start","id":{id},"level":3,"parent":0,"text":"","type":{activity_type},"fields":[]}}"#
        )
    }

    fn progress(id: u64, done: u64, expected: u64, running: u64) -> String {
        format!(
            r#"@nix {{"action":"result","id":{id},"type":105,"fields":[{done},{expected},{running},0]}}"#
        )
    }

    fn set_expected(id: u64, activity_type: u64, expected: u64) -> String {
        format!(
            r#"@nix {{"action":"result","id":{id},"type":106,"fields":[{activity_type},{expected}]}}"#
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
    fn nothing_is_drawn_before_the_first_counter_moves() {
        assert!(NixLog::new().snapshot().is_idle());
    }

    #[test]
    fn build_counts_are_accumulated() {
        let mut log = NixLog::new();
        log.observe(&start(1, ACT_BUILDS));
        log.observe(&progress(1, 3, 17, 2));

        let snapshot = log.snapshot();
        assert_eq!(snapshot.builds_done, 3);
        assert_eq!(snapshot.builds_expected, 17);
        assert_eq!(snapshot.builds_running, 2);
        assert!(!snapshot.is_idle());
    }

    #[test]
    fn download_counts_and_bytes_are_accumulated_separately() {
        let mut log = NixLog::new();
        log.observe(&start(1, ACT_COPY_PATHS));
        log.observe(&progress(1, 12, 37, 1));
        log.observe(&start(2, ACT_COPY_PATH));
        log.observe(&progress(2, 1_000, 4_000, 0));
        log.observe(&start(3, ACT_COPY_PATH));
        log.observe(&progress(3, 500, 500, 0));

        let snapshot = log.snapshot();
        assert_eq!(
            (snapshot.downloads_done, snapshot.downloads_expected),
            (12, 37)
        );
        assert_eq!(
            (snapshot.bytes_done, snapshot.bytes_expected),
            (1_500, 4_500)
        );
    }

    #[test]
    fn a_finished_activity_keeps_its_work_in_the_totals() {
        let mut log = NixLog::new();
        log.observe(&start(1, ACT_COPY_PATH));
        log.observe(&progress(1, 2_048, 2_048, 0));
        log.observe(&stop(1));
        log.observe(&start(2, ACT_COPY_PATH));
        log.observe(&progress(2, 512, 4_096, 0));

        let snapshot = log.snapshot();
        assert_eq!(snapshot.bytes_done, 2_560);
        assert_eq!(snapshot.bytes_expected, 6_144);
        assert!(log.live.len() == 1, "the stopped activity should be gone");
    }

    #[test]
    fn a_total_published_by_a_parent_is_a_floor_for_the_expectation() {
        let mut log = NixLog::new();
        log.observe(&start(1, ACT_COPY_PATH));
        log.observe(&progress(1, 0, 1_024, 0));
        log.observe(&set_expected(99, ACT_COPY_PATH, 90_000));

        assert_eq!(log.snapshot().bytes_expected, 90_000);

        // Once the activity that published it is gone, only the live expectations remain.
        log.observe(&stop(99));
        assert_eq!(log.snapshot().bytes_expected, 1_024);
    }

    #[test]
    fn a_floor_never_drags_the_expectation_below_what_is_already_known() {
        let mut log = NixLog::new();
        log.observe(&start(1, ACT_BUILDS));
        log.observe(&progress(1, 0, 17, 0));
        log.observe(&set_expected(99, ACT_BUILDS, 2));

        assert_eq!(log.snapshot().builds_expected, 17);
    }

    #[test]
    fn byte_counts_fall_back_to_raw_transfers_before_a_path_is_copied() {
        let mut log = NixLog::new();
        log.observe(&start(1, ACT_FILE_TRANSFER));
        log.observe(&progress(1, 128, 256, 0));

        assert_eq!(
            (log.snapshot().bytes_done, log.snapshot().bytes_expected),
            (128, 256)
        );

        // A copied path is the more meaningful number, so it takes over as soon as there is one.
        log.observe(&start(2, ACT_COPY_PATH));
        log.observe(&progress(2, 1, 4_096, 0));
        assert_eq!(
            (log.snapshot().bytes_done, log.snapshot().bytes_expected),
            (1, 4_096)
        );
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
            r#"@nix {"action":"start","id":1,"level":3,"text":"","type":100,"fields":["/nix/store/x","https://cache.nixos.org","local"]}"#,
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
        assert_eq!(log.snapshot().builds_expected, 2);
    }

    #[test]
    fn the_whole_stream_of_a_small_build_lands_on_sensible_totals() {
        let mut log = NixLog::new();
        for line in [
            start(1, ACT_COPY_PATHS),
            start(2, ACT_BUILDS),
            set_expected(1, ACT_COPY_PATH, 4_096),
            start(3, ACT_COPY_PATH),
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
}
