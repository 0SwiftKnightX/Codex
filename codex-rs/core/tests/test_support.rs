#![allow(clippy::expect_used)]

// Helpers shared by the integration tests.  These are located inside the
// `tests/` tree on purpose so they never become part of the public API surface
// of the `codex-core` crate.

use tempfile::TempDir;

use codex_core::config::Config;
use codex_core::config::ConfigOverrides;
use codex_core::config::ConfigToml;

/// Returns a default `Config` whose on-disk state is confined to the provided
/// temporary directory. Using a per-test directory keeps tests hermetic and
/// avoids clobbering a developer’s real `~/.codex`.
pub fn load_default_config_for_test(codex_home: &TempDir) -> Config {
    Config::load_from_base_config_with_overrides(
        ConfigToml::default(),
        ConfigOverrides::default(),
        codex_home.path().to_path_buf(),
    )
    .expect("defaults for test should always succeed")
}

/// Builds an SSE stream body from a JSON fixture.
///
/// The fixture must contain an array of objects where each object represents a
/// single SSE event with at least a `type` field matching the `event:` value.
/// Additional fields become the JSON payload for the `data:` line. An object
/// with only a `type` field results in an event with no `data:` section. This
/// makes it trivial to extend the fixtures as OpenAI adds new event kinds or
/// fields.
pub fn load_sse_fixture(path: impl AsRef<std::path::Path>) -> String {
    let events: Vec<serde_json::Value> =
        serde_json::from_reader(std::fs::File::open(path).expect("read fixture"))
            .expect("parse JSON fixture");
    events
        .into_iter()
        .map(|e| {
            let kind = e
                .get("type")
                .and_then(|v| v.as_str())
                .expect("fixture event missing type");
            if e.as_object().map(|o| o.len() == 1).unwrap_or(false) {
                format!("event: {kind}\n\n")
            } else {
                format!("event: {kind}\ndata: {e}\n\n")
            }
        })
        .collect()
}

/// Like [`load_sse_fixture`] but substitutes the placeholder `__ID__` with the
/// provided identifier before parsing. Useful when the test needs unique
/// `response_id` values.
pub fn load_sse_fixture_with_id(path: impl AsRef<std::path::Path>, id: &str) -> String {
    let raw = std::fs::read_to_string(path).expect("read fixture template");
    let replaced = raw.replace("__ID__", id);
    let events: Vec<serde_json::Value> =
        serde_json::from_str(&replaced).expect("parse JSON fixture");
    events
        .into_iter()
        .map(|e| {
            let kind = e
                .get("type")
                .and_then(|v| v.as_str())
                .expect("fixture event missing type");
            if e.as_object().map(|o| o.len() == 1).unwrap_or(false) {
                format!("event: {kind}\n\n")
            } else {
                format!("event: {kind}\ndata: {e}\n\n")
            }
        })
        .collect()
}
