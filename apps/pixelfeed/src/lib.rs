//! Pixelfeed — SDK demo combining every primitive in 2a–2e.
//!
//! 50 cards laid out vertically (`LazyColumn`), each with its own
//! 5-photo horizontal pager (`LazyRow` + `SnapMode::Page`), action
//! row of three buttons, and a multi-line caption with `max_lines`
//! ellipsis. Per-card state (which photo is showing, which card is
//! liked) lives outside `UiState` in `cards: HashMap<u32, CardAnim>`
//! so a virtualised card that re-realises picks up where it left off.

use std::collections::HashMap;

use akiva_sdk::prelude::*;
use akiva_sdk::ui::SnapMode;

const ITEM_COUNT: u32 = 50;
const PHOTOS_PER_CARD: u32 = 5;
const CARD_W: f32 = 400.0;
const PHOTO_SIZE: f32 = 400.0;
const HEADER_H: f32 = 56.0;
const ACTION_H: f32 = 44.0;
const CAPTION_H: f32 = 56.0;
const CARD_GAP: f32 = 16.0;
const CARD_ITEM_H: f32 = HEADER_H + PHOTO_SIZE + ACTION_H + CAPTION_H;
const TITLE_BAND_H: f32 = 64.0;
/// Centred LazyColumn top padding — must match the `padding_trbl`
/// passed to `lazy_column.padding_trbl` in `on_render`. Pulled out
/// as a constant so `hit_test_photo` reads the same value.
const FEED_TOP_PADDING: f32 = 8.0;

const FEED_BG: u32 = 0xfff7f8fa;
const CARD_BG: u32 = 0xffffffff;
const TEXT_FG: u32 = 0xff15172a;
const META_FG: u32 = 0xff6c7686;
const AVATAR_RING: u32 = 0xffe3e6ee;
const HEART_LIKED: u32 = 0xffe11d48;
/// Big heart overlay that crossfades over the photo on like-tap.
/// White with full opacity at peak; the Frame::opacity_av wrap
/// fades it out over the heart-overlay spring's settle time.
const HEART_OVERLAY_FG: u32 = 0xffffffff;
const HEART_OVERLAY_SIZE: f32 = 140.0;
/// Soft drop-shadow rect painted behind the overlay glyph so it
/// reads as "lifted" over arbitrary photo backgrounds.
const HEART_OVERLAY_SHADOW: u32 = 0x55000000;

/// Per-card mutable state owned by the app, NOT by the SDK. The
/// SDK's `UiState::scroll_states` already keys the photo pager
/// position by `format!("post-{i}-photos")`, so scroll position
/// survives card re-realisation for free. The state in here is the
/// stuff the SDK doesn't model: like state, the heart-overlay
/// fade-out animation, and the per-card double-tap detector.
#[derive(Default)]
struct CardAnim {
    /// `true` if this card has been liked (heart filled in red).
    liked: bool,
    /// Visible "likes" count below the action row.
    like_count: u32,
    /// Crossfade alpha for the big heart overlay painted over the
    /// photo on like-tap. `1.0` at the moment of tap, springs back
    /// to `0.0` over a few hundred ms.
    heart_overlay_alpha: AnimatedValue<f32>,
    /// Stage 3.7 — per-card image fade-in alpha. `0.0` until the
    /// card first enters the viewport, then springs to `1.0` over
    /// a soft spring (~250 ms). Once `seen` flips true the spring
    /// has been kicked off; subsequent rebuilds read the same
    /// `AnimatedValue` and the fade continues / settles naturally.
    image_alpha: AnimatedValue<f32>,
    /// `true` after the first `kick_image_fade_in` call. Prevents
    /// re-firing the spring on every realisation — once the card's
    /// image has started fading in, scrolling away and back doesn't
    /// reset the fade.
    seen: bool,
    /// Per-card double-tap detector. Owns its own armed/streak
    /// state so a tap on one card doesn't chain to the next.
    dt: DoubleTapDetector,
}

impl CardAnim {
    fn new() -> Self {
        Self {
            liked: false,
            like_count: 0,
            heart_overlay_alpha: AnimatedValue::new(0.0),
            image_alpha: AnimatedValue::new(0.0),
            seen: false,
            dt: DoubleTapDetector::new(),
        }
    }

    /// Idempotent — kick off the image fade-in spring on the first
    /// call, no-op on subsequent calls. Called from inside the
    /// LazyColumn realisation closure (only for cards entering or
    /// staying in the viewport).
    fn kick_image_fade_in(&mut self) {
        if self.seen { return; }
        self.seen = true;
        self.image_alpha.spring_to(1.0, Spring::MEDIUM);
    }

    /// Like-button tap or double-tap-to-like. Toggles `liked` and
    /// `like_count`, then kicks off the heart-overlay fade-out. The
    /// overlay alpha snaps to 1.0 instantly (no fade-in — the tap
    /// IS the moment of peak), then springs back to 0.0 over a
    /// medium spring. Re-tapping mid-fade resets the curve.
    fn on_like_pressed(&mut self) {
        if self.liked {
            self.liked = false;
            self.like_count = self.like_count.saturating_sub(1);
        } else {
            self.liked = true;
            self.like_count = self.like_count.saturating_add(1);
        }
        self.heart_overlay_alpha.set_immediately(1.0);
        self.heart_overlay_alpha.spring_to(0.0, Spring::MEDIUM);
    }

    /// Double-tap-on-photo behaves like Instagram: the gesture
    /// always LIKES (never unlikes). If the card is already liked
    /// we just re-fire the heart overlay for user feedback.
    fn on_double_tap_to_like(&mut self) {
        if !self.liked {
            self.on_like_pressed();
        } else {
            self.heart_overlay_alpha.set_immediately(1.0);
            self.heart_overlay_alpha.spring_to(0.0, Spring::MEDIUM);
        }
    }

    fn tick(&mut self, dt_ms: f32) {
        self.heart_overlay_alpha.tick(dt_ms);
        self.image_alpha.tick(dt_ms);
    }
}

#[derive(Default)]
struct PixelfeedMain {
    ui: UiState,
    /// Per-card animation/like state, keyed by post index. Survives
    /// LazyColumn realisation churn.
    cards: HashMap<u32, CardAnim>,
    /// Wall-clock timestamp of the previous `on_render`, used for
    /// per-frame `dt_ms` driving the heart-overlay tick. `0.0`
    /// until the first frame.
    last_ts: f64,
    /// Last-known feed scroll-y, captured at the end of each
    /// `on_render`. Read by `on_pointer` to geometrically map an
    /// event's screen coords back to a card index without needing
    /// the SDK's `Ui` (which only exists during render).
    last_scroll_y: f32,
    /// Last-known viewport width, captured at the end of each
    /// `on_render`. Same rationale as `last_scroll_y`. `0.0` until
    /// the first render; in that window `on_pointer` skips
    /// double-tap detection.
    last_viewport_w: f32,
}

#[async_trait(?Send)]
impl Activity for PixelfeedMain {
    async fn on_create(&mut self, ctx: &mut Context<'_>) {
        log_info!(
            "Pixelfeed created ({}x{} @ {:.1}x)",
            ctx.metrics.width_px, ctx.metrics.height_px, ctx.metrics.density,
        );
    }

    async fn on_pointer(&mut self, ctx: &mut Context<'_>, ev: PointerEvent) {
        // Standard pipeline: hit-test for buttons, drag for scroll.
        self.ui.feed_pointer(ev);

        // Per-card double-tap routing. We derive which card's photo
        // region contains the event from the cached layout-state
        // and route ONLY that card's detector. Cards whose photos
        // don't contain the event stay untouched, so a tap that
        // bounces between cards doesn't accidentally fuse.
        if self.last_viewport_w <= 0.0 { return; }
        let Some(i) = hit_test_photo(
            ev.x, ev.y, self.last_viewport_w, self.last_scroll_y,
        ) else {
            return;
        };
        let now = ctx.timestamp_ms;
        let anim = self.cards.entry(i).or_insert_with(CardAnim::new);
        if anim.dt.feed(ev, now).is_some() {
            anim.on_double_tap_to_like();
        }
    }

    async fn on_wheel(&mut self, _ctx: &mut Context<'_>, ev: WheelEvent) {
        self.ui.feed_wheel(ev);
    }

    async fn on_render(&mut self, ctx: &mut Context<'_>, c: &mut Canvas<'_>) {
        c.clear(FEED_BG);

        // Per-frame dt for the heart-overlay spring tick. First
        // frame (last_ts == 0.0) seeds; subsequent frames advance
        // every card's animation by the wall-clock delta.
        let dt_ms = if self.last_ts > 0.0 {
            (ctx.timestamp_ms - self.last_ts) as f32
        } else {
            0.0
        };
        if dt_ms > 0.0 {
            for anim in self.cards.values_mut() {
                anim.tick(dt_ms);
            }
        }
        self.last_ts = ctx.timestamp_ms;

        let viewport_w = ctx.metrics.width_px as f32;
        let viewport_h = ctx.metrics.height_px as f32;

        // Split borrow so the per-card closure can mutate `cards`
        // while `Ui` holds &mut `self.ui`.
        let cards = &mut self.cards;
        let mut ui = Ui::new(&mut self.ui, ctx, c);

        // ── Title strip ──────────────────────────────────────────
        let title = ui.text("Akiva Pixelfeed")
            .size(22.0).color(TEXT_FG).build();
        let subtitle = ui.text("scroll · swipe photos · double-tap to like")
            .size(13.0).color(META_FG).build();
        let title_strip = ui.v_stack(&[title, subtitle])
            .gap(4.0).padding_trbl(20.0, 16.0, 12.0, 16.0).build();

        // ── Feed body ────────────────────────────────────────────
        let feed_h = (viewport_h - TITLE_BAND_H).max(0.0);

        let list = ui.lazy_column(ITEM_COUNT)
            .key("feed")
            .item_h(CARD_ITEM_H)
            .gap(CARD_GAP)
            .padding_trbl(FEED_TOP_PADDING, 0.0, 16.0, 0.0)
            .size(viewport_w, feed_h)
            .items(|ui, i| {
                let anim = cards.entry(i).or_insert_with(CardAnim::new);
                // The realisation closure is called only for cards
                // entering or staying in the viewport (LazyColumn
                // virtualises the rest), so it's the natural place
                // to fire the image fade-in. `kick_image_fade_in`
                // is idempotent — only the first call per card
                // starts the spring; subsequent rebuilds let the
                // spring continue ticking from `on_render`.
                anim.kick_image_fade_in();
                post_card(ui, i, anim)
            });

        let root = ui.v_stack(&[title_strip, list])
            .gap(0.0)
            .cross(CrossAxis::Stretch)
            .build();
        ui.present(root);

        // ── Handle button clicks (post-`present`) ────────────────
        for (&i, anim) in cards.iter_mut() {
            let like_id = format!("like-{i}");
            if ui.was_clicked(&like_id) {
                anim.on_like_pressed();
            }
        }

        // Capture layout snapshot for next frame's `on_pointer`.
        self.last_scroll_y = ui.scroll_offset("feed");
        self.last_viewport_w = viewport_w;
    }
}

/// Geometrically map an event coord to a card index, returning
/// `Some(i)` only if `(ev_x, ev_y)` lands inside card `i`'s photo
/// region in the current frame's layout.
fn hit_test_photo(
    ev_x: f32, ev_y: f32, viewport_w: f32, scroll_y: f32,
) -> Option<u32> {
    let photo_left = (viewport_w - CARD_W) / 2.0;
    if ev_x < photo_left || ev_x >= photo_left + CARD_W {
        return None;
    }
    let content_y = ev_y - TITLE_BAND_H - FEED_TOP_PADDING + scroll_y;
    if content_y < 0.0 { return None; }
    let stride = CARD_ITEM_H + CARD_GAP;
    let card_idx = (content_y / stride).floor() as i64;
    if card_idx < 0 || card_idx >= ITEM_COUNT as i64 { return None; }
    let i = card_idx as u32;
    let y_in_card = content_y - card_idx as f32 * stride;
    if y_in_card < HEADER_H || y_in_card >= HEADER_H + PHOTO_SIZE {
        return None;
    }
    Some(i)
}

fn post_card(ui: &mut Ui<'_, '_, '_>, i: u32, anim: &mut CardAnim) -> NodeId {
    let header = build_header(ui, i);
    let photo_pager = build_photo_pager(ui, i, anim);
    let action_row = build_action_row(ui, i, anim);
    let caption = build_caption(ui, i, anim);

    let inner = ui.v_stack(&[header, photo_pager, action_row, caption])
        .gap(0.0)
        .cross(CrossAxis::Stretch)
        .build();

    let bg = ui.rect(CARD_BG).radius(14.0).build();
    let card = ui.frame(&[bg, inner])
        .size(CARD_W, CARD_ITEM_H)
        .clip_radius(14.0)
        .build();

    ui.h_stack(&[card])
        .main(MainAxis::Center)
        .cross(CrossAxis::Stretch)
        .build()
}

fn build_header(ui: &mut Ui<'_, '_, '_>, i: u32) -> NodeId {
    const AVATAR_PALETTE: [u32; 6] = [
        0xff2256d3, 0xfff05d4a, 0xff15b97a,
        0xff8c5cf6, 0xfff15ba0, 0xff14b87b,
    ];
    let avatar_color = AVATAR_PALETTE[i as usize % AVATAR_PALETTE.len()];

    let avatar_inner = ui.rect(avatar_color).size(40.0, 40.0).build();
    let avatar = ui.frame(&[avatar_inner])
        .size(40.0, 40.0)
        .clip_radius(20.0)
        .build();
    let avatar_ring = ui.rect(AVATAR_RING).size(48.0, 48.0).radius(24.0).build();
    let avatar_stack = ui.frame(&[avatar_ring, avatar])
        .size(48.0, 48.0)
        .build();

    let username = ui.text(&format!("user_{:02}", i + 1))
        .size(14.0).color(TEXT_FG).build();
    let meta = ui.text("Akiva HQ · 2026")
        .size(11.0).color(META_FG).build();
    let name_stack = ui.v_stack(&[username, meta])
        .gap(2.0).build();

    let spacer = ui.spacer().grow(1.0).build();
    let menu = ui.text("•••").size(18.0).color(META_FG).build();

    ui.h_stack(&[avatar_stack, name_stack, spacer, menu])
        .gap(10.0).padding_trbl(8.0, 12.0, 8.0, 12.0)
        .cross(CrossAxis::Center)
        .build()
}

/// Build the photo-pager Frame. The pager itself is a LazyRow with
/// per-card scroll key; we wrap it in a Frame that ALSO carries
/// the big heart-overlay glyph (alpha-driven by
/// `anim.heart_overlay_alpha`) layered on top. The overlay's Frame
/// uses `opacity_av` so when the spring settles back to 0.0 the
/// pair takes the no-op fast path (no push/pop_opacity ops emitted).
fn build_photo_pager(ui: &mut Ui<'_, '_, '_>, i: u32, anim: &CardAnim) -> NodeId {
    let key = format!("post-{i}-photos");
    let pager = ui.lazy_row(PHOTOS_PER_CARD)
        .key(&key)
        .item_w(PHOTO_SIZE)
        .gap(0.0)
        .size(CARD_W, PHOTO_SIZE)
        .snap(SnapMode::Page)
        .items(move |ui, j| photo_block(ui, i, j));

    // Stage 3.7 — wrap the pager in a Frame whose opacity is driven
    // by `anim.image_alpha`. Cards just entering the viewport
    // start at 0.0 and spring to 1.0 over ~250 ms. Once
    // `image_alpha.is_settled()` at 1.0, `Frame::opacity_av(1.0)`
    // takes the no-op fast path (no push/pop_opacity emitted).
    let pager_faded = ui.frame(&[pager])
        .size(CARD_W, PHOTO_SIZE)
        .opacity_av(&anim.image_alpha)
        .build();

    // Heart overlay — painted only while the alpha is non-trivially
    // visible. `Frame::opacity_av` reads `anim.heart_overlay_alpha`
    // at build time; when it's <= some threshold we skip the
    // overlay entirely so a settled card pays zero overhead.
    //
    // The overlay sits OUTSIDE the image_alpha layer so a freshly-
    // entered card whose image is still fading in can still show
    // the like-pop heart at full opacity (the user did just tap).
    let alpha_now = anim.heart_overlay_alpha.value();
    if alpha_now < 0.02 {
        return pager_faded;
    }
    let overlay = build_heart_overlay(ui, anim);
    ui.frame(&[pager_faded, overlay])
        .size(CARD_W, PHOTO_SIZE)
        .build()
}

/// Build the centred heart overlay. Soft shadow circle + the heart
/// glyph, both wrapped in a single opacity layer driven by the
/// animated value. Centred via `MainAxis::Center` + `CrossAxis::Center`.
fn build_heart_overlay(ui: &mut Ui<'_, '_, '_>, anim: &CardAnim) -> NodeId {
    let shadow_inner = ui.rect(HEART_OVERLAY_SHADOW)
        .size(HEART_OVERLAY_SIZE + 16.0, HEART_OVERLAY_SIZE + 16.0)
        .build();
    let shadow = ui.frame(&[shadow_inner])
        .size(HEART_OVERLAY_SIZE + 16.0, HEART_OVERLAY_SIZE + 16.0)
        .clip_radius((HEART_OVERLAY_SIZE + 16.0) / 2.0)
        .build();
    let heart = ui.text("♥")
        .size(HEART_OVERLAY_SIZE * 0.8)
        .color(HEART_OVERLAY_FG)
        .build();
    let stack = ui.frame(&[shadow, heart])
        .size(HEART_OVERLAY_SIZE + 16.0, HEART_OVERLAY_SIZE + 16.0)
        .build();
    // Centre the overlay inside the photo via two stacks. The
    // outer Frame opacity_av wraps the whole thing in the spring-
    // driven alpha layer.
    let h_centered = ui.h_stack(&[stack])
        .main(MainAxis::Center)
        .cross(CrossAxis::Center)
        .build();
    let v_centered = ui.v_stack(&[h_centered])
        .main(MainAxis::Center)
        .cross(CrossAxis::Stretch)
        .build();
    ui.frame(&[v_centered])
        .size(CARD_W, PHOTO_SIZE)
        .opacity_av(&anim.heart_overlay_alpha)
        .build()
}

fn photo_block(ui: &mut Ui<'_, '_, '_>, card: u32, photo: u32) -> NodeId {
    const PALETTE: [u32; 12] = [
        0xff1f3a8a, 0xfff97316, 0xff047857, 0xffb91c1c,
        0xff7c3aed, 0xff0891b2, 0xffdb2777, 0xff15803d,
        0xfff59e0b, 0xff4f46e5, 0xff334155, 0xffe11d48,
    ];
    let h = (card.wrapping_mul(7).wrapping_add(photo.wrapping_mul(3))) as usize;
    let bg_color = PALETTE[h % PALETTE.len()];
    let accent_color = PALETTE[(h + 5) % PALETTE.len()];

    let bg = ui.rect(bg_color).size(PHOTO_SIZE, PHOTO_SIZE).build();
    let band_h = 36.0;
    let band = ui.rect(accent_color).size(PHOTO_SIZE, band_h).build();
    let band_layer = ui.frame(&[band])
        .padding_trbl(40.0, 0.0, 0.0, 0.0)
        .build();
    let circle_inner = ui.rect(accent_color).size(140.0, 140.0).build();
    let circle = ui.frame(&[circle_inner])
        .size(140.0, 140.0)
        .clip_radius(70.0)
        .build();
    let circle_layer = ui.frame(&[circle])
        .padding_trbl(PHOTO_SIZE - 160.0, 20.0, 0.0, PHOTO_SIZE - 160.0)
        .build();
    ui.frame(&[bg, band_layer, circle_layer]).build()
}

fn build_action_row(ui: &mut Ui<'_, '_, '_>, i: u32, anim: &CardAnim) -> NodeId {
    let heart_label = if anim.liked { "♥" } else { "♡" };
    let _heart_color = if anim.liked { HEART_LIKED } else { TEXT_FG };
    // Using a plain Ui::button so was_clicked routes through the
    // standard hit-test pipeline. The label colour and glyph
    // already differ between liked/unliked states — sufficient
    // visual signal even without the per-button bg styling that
    // the SDK doesn't expose yet.
    let like_id = format!("like-{i}");
    let heart_btn = ui.button(&like_id, heart_label);
    ui.corner_radius(heart_btn, 8.0);

    let comment = ui.text("💬").size(20.0).color(TEXT_FG).build();
    let share = ui.text("↗").size(22.0).color(TEXT_FG).build();

    let spacer = ui.spacer().grow(1.0).build();
    ui.h_stack(&[heart_btn, comment, share, spacer])
        .gap(16.0)
        .padding_trbl(8.0, 14.0, 8.0, 14.0)
        .cross(CrossAxis::Center)
        .build()
}

fn build_caption(ui: &mut Ui<'_, '_, '_>, i: u32, anim: &CardAnim) -> NodeId {
    let likes_text = match anim.like_count {
        0 => "Be the first to like this".to_string(),
        1 => "1 like".to_string(),
        n => format!("{n} likes"),
    };
    let likes_line = ui.text(&likes_text)
        .size(12.0).color(TEXT_FG).build();

    const CAPTIONS: [&str; 8] = [
        "user_01 building akiva on a quiet sunday morning — every primitive composes today.",
        "carousels stacked inside columns, springs driving the like-pop, opacity layers fading in.",
        "the SDK toolkit ships exactly the widgets a feed needs and nothing more.",
        "fling, snap, spring — three animation primitives, one virtualised list.",
        "every pixel here is owned by the SDK; no overflow:scroll, no DOM scrollbars.",
        "double-tap the photo to like — the heart pops via a critically-damped spring.",
        "horizontal pagers per card share the same LazyRow that powers the carousel demo.",
        "Stage 3 is the integration test: all primitives composed in one app, one frame.",
    ];
    let caption_text = CAPTIONS[i as usize % CAPTIONS.len()];
    let caption_line = ui.text(caption_text)
        .size(13.0).color(META_FG).max_lines(2).build();

    ui.v_stack(&[likes_line, caption_line])
        .gap(4.0)
        .padding_trbl(4.0, 14.0, 12.0, 14.0)
        .cross(CrossAxis::Stretch)
        .build()
}

akiva_sdk::akiva_app!(PixelfeedMain);
