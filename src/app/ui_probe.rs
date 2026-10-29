use egui::Rect;
use std::cell::RefCell;
use std::collections::BTreeMap;

thread_local! {
    static RECTS: RefCell<BTreeMap<&'static str, Rect>> =
        const { RefCell::new(BTreeMap::new()) };
}

pub(crate) fn record(key: &'static str, rect: Rect) {
    RECTS.with(|rects| {
        rects.borrow_mut().insert(key, rect);
    });
}

pub(crate) fn rect(key: &'static str) -> Option<Rect> {
    RECTS.with(|rects| rects.borrow().get(key).copied())
}

pub(crate) fn clear() {
    RECTS.with(|rects| rects.borrow_mut().clear());
}
