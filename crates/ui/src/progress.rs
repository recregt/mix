use std::sync::Arc;

use indicatif::ProgressStyle;
use mix_core::{DownloadProgress, StepObserver};
use tracing::field::{Field, Visit};
use tracing_indicatif::IndicatifLayer;
use tracing_indicatif::span_ext::IndicatifSpanExt;
use tracing_subscriber::Layer;
use tracing_subscriber::field::RecordFields;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::fmt::FormatFields;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

const DOWNLOAD_STYLE: &str = "{span_child_prefix}{spinner:.cyan} {span_fields} {bytes}/{total_bytes} ({binary_bytes_per_sec})";

const OWN_CRATES: [&str; 4] = ["mix_core", "mix_app", "mix_cli", "mix_ui"];

const TICK_CHARS: &str = "⠁⠁⠉⠙⠚⠒⠂⠂⠒⠲⠴⠤⠄⠄⠤⠠⠠⠤⠦⠖⠒⠐⠐⠒⠓⠋⠉⠈⠈✓";

fn level_filter(verbosity: u8) -> LevelFilter {
    match verbosity {
        0 => LevelFilter::WARN,
        1 => LevelFilter::INFO,
        2 => LevelFilter::DEBUG,
        _ => LevelFilter::TRACE,
    }
}

struct NameOnlyFields;

impl<'writer> FormatFields<'writer> for NameOnlyFields {
    fn format_fields<R: RecordFields>(
        &self,
        mut writer: Writer<'writer>,
        fields: R,
    ) -> std::fmt::Result {
        struct NameVisitor<'a, 'w> {
            writer: &'a mut Writer<'w>,
        }

        impl Visit for NameVisitor<'_, '_> {
            fn record_str(&mut self, field: &Field, value: &str) {
                if field.name() == "name" {
                    let _ = write!(self.writer, "{value}");
                }
            }

            fn record_debug(&mut self, _field: &Field, _value: &dyn std::fmt::Debug) {}
        }

        fields.record(&mut NameVisitor {
            writer: &mut writer,
        });
        Ok(())
    }
}

fn own_crates_only() -> Targets {
    OWN_CRATES
        .iter()
        .fold(Targets::new(), |targets, name| {
            targets.with_target(*name, LevelFilter::TRACE)
        })
        .with_default(LevelFilter::OFF)
}

struct IndicatifDownloadProgress;

impl DownloadProgress for IndicatifDownloadProgress {
    fn set_total(&self, total: u64) {
        let span = tracing::Span::current();
        span.pb_set_style(
            &ProgressStyle::with_template(DOWNLOAD_STYLE)
                .expect("progress bar template is valid")
                .tick_chars(TICK_CHARS),
        );
        span.pb_set_length(total);
    }

    fn add(&self, delta: u64) {
        tracing::Span::current().pb_inc(delta);
    }
}

pub fn download_reporter() -> Arc<dyn DownloadProgress> {
    Arc::new(IndicatifDownloadProgress)
}

struct IndicatifStepObserver;

impl StepObserver for IndicatifStepObserver {
    fn on_step_span(&self, span: &tracing::Span) {
        // Persist the line instead of clearing it: `finish_using_style` always forces an
        // immediate draw regardless of the redraw rate limiter, so even a step that completes
        // in a millisecond is guaranteed to render before its line disappears.
        span.pb_set_finish_message("");
    }
}

pub fn step_observer() -> Arc<dyn StepObserver> {
    Arc::new(IndicatifStepObserver)
}

pub fn init_tracing(verbosity: u8) {
    let indicatif_layer = IndicatifLayer::new()
        .with_span_field_formatter(NameOnlyFields)
        .with_progress_style(
            ProgressStyle::with_template("{span_child_prefix}{spinner:.cyan} {span_fields}")
                .expect("progress bar template is valid")
                .tick_chars(TICK_CHARS),
        );

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(indicatif_layer.get_stderr_writer())
                .without_time()
                .with_target(false)
                .with_filter(level_filter(verbosity)),
        )
        .with(indicatif_layer.with_filter(own_crates_only()))
        .init();
}
