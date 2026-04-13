use gpui::*;
use std::cell::Cell;
use std::rc::Rc;
use std::time::Instant;

// ── Constants ──────────────────────────────────────────────────

pub const TRACK_SIZE: f32 = 14.0;
pub const THUMB_WIDTH: f32 = 6.0;
pub const THUMB_ACTIVE_WIDTH: f32 = 8.0;
pub const THUMB_INSET: f32 = 4.0;
pub const THUMB_RADIUS: f32 = 3.0;
pub const THUMB_ACTIVE_RADIUS: f32 = 4.0;
pub const MIN_THUMB_SIZE: f32 = 48.0;
pub const FADE_OUT_DELAY: f32 = 2.0;
pub const FADE_OUT_DURATION: f32 = 3.0;

// ── State ──────────────────────────────────────────────────────

#[derive(Clone, Copy)]
struct Inner {
    dragging: bool,
    drag_start_y: f32,
    hovered: bool,
    hovered_thumb: bool,
    last_scroll_time: Option<Instant>,
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            dragging: false,
            drag_start_y: 0.0,
            hovered: false,
            hovered_thumb: false,
            last_scroll_time: None,
        }
    }
}

/// Shared scrollbar state. Clone-cheap (Rc).
#[derive(Clone)]
pub struct ScrollbarState(Rc<Cell<Inner>>);

impl Default for ScrollbarState {
    fn default() -> Self {
        Self::new()
    }
}

impl ScrollbarState {
    pub fn new() -> Self {
        Self(Rc::new(Cell::new(Inner::default())))
    }

    fn get(&self) -> Inner {
        self.0.get()
    }
    fn set(&self, v: Inner) {
        self.0.set(v);
    }

    /// Mark that a scroll just happened (shows the scrollbar with fade timer).
    pub fn did_scroll(&self) {
        let mut s = self.get();
        s.last_scroll_time = Some(Instant::now());
        self.set(s);
    }

    fn is_visible(&self) -> bool {
        let s = self.get();
        if s.dragging || s.hovered {
            return true;
        }
        match s.last_scroll_time {
            None => false,
            Some(t) => Instant::now().duration_since(t).as_secs_f32() < FADE_OUT_DURATION,
        }
    }

    fn opacity(&self) -> f32 {
        let s = self.get();
        if s.dragging || s.hovered {
            return 1.0;
        }
        match s.last_scroll_time {
            None => 0.0,
            Some(t) => {
                let elapsed = Instant::now().duration_since(t).as_secs_f32();
                if elapsed < FADE_OUT_DELAY {
                    1.0
                } else if elapsed < FADE_OUT_DURATION {
                    1.0 - ((elapsed - FADE_OUT_DELAY)
                        / (FADE_OUT_DURATION - FADE_OUT_DELAY))
                } else {
                    0.0
                }
            }
        }
    }
}

// ── Paint helper ───────────────────────────────────────────────

/// Paint a vertical scrollbar inside `viewport_bounds` using the given
/// `scroll_handle` for position/size info. Call this from a `canvas()`
/// paint callback or `deferred()` layer.
///
/// `scroll_handle` must be the same handle passed to `.track_scroll()`
/// on the scrollable container.
pub fn paint_scrollbar(
    viewport_bounds: Bounds<Pixels>,
    scroll_handle: &ScrollHandle,
    state: &ScrollbarState,
    window: &mut Window,
) {
    let offset = scroll_handle.offset();
    let max_offset = scroll_handle.max_offset();
    let content_h: f32 = (max_offset.height + viewport_bounds.size.height).into();
    let viewport_h: f32 = viewport_bounds.size.height.into();

    // Nothing to scroll
    if content_h <= viewport_h {
        return;
    }

    let sb = state.get();
    let is_visible = sb.dragging || sb.hovered || state.is_visible();
    if !is_visible {
        return;
    }

    let track_h = viewport_h;
    let thumb_h = (viewport_h / content_h * track_h)
        .max(MIN_THUMB_SIZE)
        .min(track_h);
    let scroll_ratio = f32::from(-offset.y) / (content_h - viewport_h);
    let thumb_top = scroll_ratio * (track_h - thumb_h);

    let is_active = sb.dragging || sb.hovered_thumb;
    let tw = if is_active {
        THUMB_ACTIVE_WIDTH
    } else {
        THUMB_WIDTH
    };

    let thumb_bounds = Bounds::new(
        point(
            viewport_bounds.origin.x + viewport_bounds.size.width
                - px(THUMB_INSET + tw),
            viewport_bounds.origin.y + px(thumb_top),
        ),
        size(px(tw), px(thumb_h)),
    );

    let (color, radius) = if sb.dragging || sb.hovered_thumb {
        (hsla(0.0, 0.0, 1.0, 0.5), px(THUMB_ACTIVE_RADIUS))
    } else if sb.hovered {
        (hsla(0.0, 0.0, 1.0, 0.4), px(THUMB_ACTIVE_RADIUS))
    } else {
        (
            hsla(0.0, 0.0, 1.0, state.opacity() * 0.35),
            px(THUMB_RADIUS),
        )
    };

    window.paint_quad(fill(thumb_bounds, color).corner_radii(radius));

    // ── Mouse handlers ─────────────────────────────────────────

    let track_bounds = Bounds::new(
        point(
            viewport_bounds.origin.x + viewport_bounds.size.width
                - px(TRACK_SIZE),
            viewport_bounds.origin.y,
        ),
        size(px(TRACK_SIZE), viewport_bounds.size.height),
    );

    let scroll = scroll_handle.clone();
    let max_scroll = content_h - viewport_h;

    // MouseDown: start drag on thumb, or jump on track click
    window.on_mouse_event({
        let state = state.clone();
        let scroll = scroll.clone();
        move |event: &MouseDownEvent, phase, window, _cx| {
            if !phase.bubble() {
                return;
            }
            if !track_bounds.contains(&event.position) {
                return;
            }

            let mut s = state.get();
            s.last_scroll_time = Some(Instant::now());

            if thumb_bounds.contains(&event.position) {
                s.dragging = true;
                s.drag_start_y =
                    f32::from(event.position.y - thumb_bounds.origin.y);
            } else {
                let pct = ((f32::from(event.position.y)
                    - f32::from(track_bounds.origin.y)
                    - thumb_h / 2.0)
                    / (track_h - thumb_h))
                    .clamp(0.0, 1.0);
                let new_y = -(pct * max_scroll);
                scroll.set_offset(point(scroll.offset().x, px(new_y)));
            }
            state.set(s);
            window.refresh();
        }
    });

    // MouseMove: hover states + drag scrolling
    window.on_mouse_event({
        let state = state.clone();
        let scroll = scroll.clone();
        move |event: &MouseMoveEvent, _phase, window, _cx| {
            let mut s = state.get();
            let mut changed = false;

            let was_hovered = s.hovered;
            s.hovered = track_bounds.contains(&event.position);
            if s.hovered != was_hovered {
                if s.hovered {
                    s.last_scroll_time = Some(Instant::now());
                }
                changed = true;
            }

            let was_thumb = s.hovered_thumb;
            s.hovered_thumb = thumb_bounds.contains(&event.position);
            if s.hovered_thumb != was_thumb {
                changed = true;
            }

            if s.dragging && event.dragging() {
                let pct = ((f32::from(event.position.y)
                    - s.drag_start_y
                    - f32::from(track_bounds.origin.y))
                    / (track_h - thumb_h))
                    .clamp(0.0, 1.0);
                let new_y = -(pct * max_scroll);
                scroll.set_offset(point(scroll.offset().x, px(new_y)));
                s.last_scroll_time = Some(Instant::now());
                changed = true;
            }

            if changed {
                state.set(s);
                window.refresh();
            }
        }
    });

    // MouseUp: end drag
    window.on_mouse_event({
        let state = state.clone();
        move |_event: &MouseUpEvent, phase, window, _cx| {
            if !phase.bubble() {
                return;
            }
            let mut s = state.get();
            if s.dragging {
                s.dragging = false;
                state.set(s);
                window.refresh();
            }
        }
    });
}
