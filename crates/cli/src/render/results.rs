use mix_events::v1::node_finished::Result;
use mix_shell::profile::state::Source;

use crate::explain::change;

pub(super) fn finished(result: &Result, printed: bool) {
    match result {
        Result::Install(install) => {
            reset(install.restored);
            if printed {
                changed(
                    &install.added,
                    &install.skipped,
                    "already installed",
                    "nothing to install",
                    "installed",
                );
            }
        }
        Result::Remove(remove) => {
            reset(remove.restored);
            if printed {
                changed(
                    &remove.removed,
                    &remove.skipped,
                    "not installed",
                    "nothing to remove",
                    "removed",
                );
            }
        }
        _ => {}
    }
}

fn reset(was_reset: bool) {
    if was_reset && let Some(note) = change::restored(Source::Fresh) {
        mix_ui::warn(note.message());
    }
}

fn changed(changed: &[String], skipped: &[String], skipped_as: &str, none: &str, done: &str) {
    if !skipped.is_empty() {
        mix_ui::skipped(format!("{skipped_as}: {}", skipped.join(", ")));
    }
    if changed.is_empty() {
        mix_ui::ok(none);
    } else {
        mix_ui::ok(format!("{done}: {}", changed.join(", ")));
    }
}
