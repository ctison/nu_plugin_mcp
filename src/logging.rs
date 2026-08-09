use std::sync::OnceLock;

use tracing::level_filters::LevelFilter;
use tracing_subscriber::{Registry, layer::SubscriberExt, reload, util::SubscriberInitExt as _};

static LOG_FILTER: OnceLock<reload::Handle<LevelFilter, Registry>> = OnceLock::new();

/// Installs stderr tracing with a reloadable info-level filter.
pub fn initialize() {
    let (filter, handle) = reload::Layer::new(LevelFilter::INFO);
    let subscriber = Registry::default().with(filter).with(
        tracing_subscriber::fmt::layer()
            .with_writer(std::io::stderr)
            .with_ansi(false),
    );

    if subscriber.try_init().is_ok() {
        let _ = LOG_FILTER.set(handle);
    }
}

/// Updates the process-wide tracing filter when this binary installed it.
pub fn set_level(level: LevelFilter) -> Result<(), String> {
    LOG_FILTER
        .get()
        .map_or(Ok(()), |handle| handle.reload(level))
        .map_err(|error| error.to_string())
}
