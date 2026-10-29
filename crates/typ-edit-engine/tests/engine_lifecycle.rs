use std::sync::Arc;
use std::time::{Duration, Instant};

use typ_edit_engine::Engine;
use typ_edit_engine::fonts::{FontCatalog, Fonts};
use typ_edit_model::{BytePos, ParseRequest, TextSnapshot};

#[test]
fn engine_shuts_down_promptly_and_ignores_late_requests() {
    let mut engine = Engine::spawn(Arc::new(|| {}), Fonts::ready(FontCatalog::new()));

    let events = engine.drain();
    assert!(events.parses.is_empty());
    assert!(events.compiles.is_empty());
    assert!(events.raster.is_empty());
    assert!(events.spells.is_empty());
    assert!(events.searches.is_empty());
    assert!(events.ide.is_empty());
    assert!(events.health.is_empty());

    let started = Instant::now();
    engine.shutdown();
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "shutdown took {elapsed:?}"
    );

    engine.request_parse(ParseRequest {
        generation: 1,
        tab: 0,
        text: TextSnapshot::from_owned(String::new(), 0),
        cursor: BytePos(0),
        want_spell: false,
    });
    let after = engine.drain();
    assert!(
        after.parses.is_empty(),
        "requests after shutdown must be no-ops"
    );
}
