//! Pixelfeed — SDK demo combining every primitive in 2a–2e.
//!
//! 50 cards laid out vertically (`LazyColumn`), each with its own
//! 5-photo horizontal pager (`LazyRow` + `SnapMode::Page`), action
//! row of three buttons, and a multi-line caption with `max_lines`
//! ellipsis. Per-card state (which photo is showing, which card is
//! liked) lives outside `UiState` in `cards: HashMap<u32, CardAnim>`
//! so a virtualised card that re-realises picks up where it left off.
//!
//! This stone (3.5) lands the static visual skeleton. Stones 3.6
//! and 3.7 wire in:
//!   - heart-pop animation (Spring on the heart icon's scale)
//!   - double-tap-to-like via `DoubleTapDetector`
//!   - per-card image fade-in via `Frame::opacity_av`

use std::collections::HashMap;

use akiva_sdk::prelude::*;
use akiva_sdk::ui::SnapMode;

const ITEM_COUNT: u32 = 50;
const PHOTOS_PER_CARD: u32 = 5;
const CARD_W: f32 = 400.0;           // narrow centred column (phone-feed look)
const PHOTO_SIZE: f32 = 400.0;       // square photos, matches CARD_W
const HEADER_H: f32 = 56.0;
const ACTION_H: f32 = 44.0;
const CAPTION_H: f32 = 56.0;
const CARD_GAP: f32 = 16.0;
const CARD_ITEM_H: f32 = HEADER_H + PHOTO_SIZE + ACTION_H + CAPTION_H;

const FEED_BG: u32 = 0xfff7f8fa;
const CARD_BG: u32 = 0xffffffff;
const TEXT_FG: u32 = 0xff15172a;
const META_FG: u32 = 0xff6c7686;
const AVATAR_RING: u32 = 0xffe3e6ee;

/// Per-card mutable state owned by the app, NOT by the SDK. The
/// SDK's `UiState::scroll_states` already keys the photo pager
/// position by `format!("post-{i}-photos")`, so scroll position
/// survives card re-realisation for free. The state in here is the
/// stuff the SDK doesn't model: like state, animation values, and
/// the per-card double-tap detector.
///
/// Wired in stones 3.6 / 3.7 — for 3.5 we only construct
/// `CardAnim::default()` to anchor the storage shape.
#[derive(Default, Clone, Copy)]
struct CardAnim {
    /// `true` if this card has been liked (heart filled in).
    liked: bool,
    /// Visible "likes" count below the action row.
    like_count: u32,
    // Stone 3.6 will populate: heart_scale, heart_phase, dt.
    // Stone 3.7 will populate: image_alpha, seen.
}

#[derive(Default)]
struct PixelfeedMain {
    ui: UiState,
    /// Per-card animation/like state, keyed by post index. Survives
    /// LazyColumn realisation churn — the entry stays once any
    /// frame has seen card `i`.
    cards: HashMap<u32, CardAnim>,
    /// Wall-clock timestamp of the previous `on_render`. Used for
    /// per-frame dt when stone 3.6 wires the heart-pop spring.
    last_ts: f64,
}

#[async_trait(?Send)]
impl Activity for PixelfeedMain {
    async fn on_create(&mut self, ctx: &mut Context<'_>) {
        log_info!(
            "Pixelfeed created ({}x{} @ {:.1}x)",
            ctx.metrics.width_px, ctx.metrics.height_px, ctx.metrics.density,
        );
    }

    async fn on_pointer(&mut self, _ctx: &mut Context<'_>, ev: PointerEvent) {
        // 3.6 routes double-tap detection per card by hit-testing
        // against the photo rect. For 3.5 we only feed the standard
        // pipeline so scroll/wheel/button work end-to-end.
        self.ui.feed_pointer(ev);
    }

    async fn on_wheel(&mut self, _ctx: &mut Context<'_>, ev: WheelEvent) {
        self.ui.feed_wheel(ev);
    }

    async fn on_render(&mut self, ctx: &mut Context<'_>, c: &mut Canvas<'_>) {
        c.clear(FEED_BG);

        // dt bookkeeping for the spring tick that 3.6 will add. 0.0
        // is the safe first-frame sentinel — timestamps grow from
        // there, so `dt = ts - last_ts` is positive on every frame
        // after the first.
        self.last_ts = ctx.timestamp_ms;

        let viewport_w = ctx.metrics.width_px as f32;
        let viewport_h = ctx.metrics.height_px as f32;

        // Split borrow so the per-card closure can mutate `cards`
        // while `Ui` holds &mut `self.ui`. This is the borrow-split
        // pattern documented in stage-3-pixelfeed-plan.md §3.1.
        let cards = &mut self.cards;
        let mut ui = Ui::new(&mut self.ui, ctx, c);

        // ── Title strip ──────────────────────────────────────────
        let title = ui.text("Akiva Pixelfeed")
            .size(22.0).color(TEXT_FG).build();
        let subtitle = ui.text("scroll · swipe photos · tap heart")
            .size(13.0).color(META_FG).build();
        let title_strip = ui.v_stack(&[title, subtitle])
            .gap(4.0).padding_trbl(20.0, 16.0, 12.0, 16.0).build();

        // ── Feed body ────────────────────────────────────────────
        // Title strip is ~64 dp; LazyColumn fills the remainder.
        let title_band_h: f32 = 64.0;
        let feed_h = (viewport_h - title_band_h).max(0.0);

        let list = ui.lazy_column(ITEM_COUNT)
            .key("feed")
            .item_h(CARD_ITEM_H)
            .gap(CARD_GAP)
            .padding_trbl(8.0, 0.0, 16.0, 0.0)
            .size(viewport_w, feed_h)
            .items(|ui, i| {
                let anim = *cards.entry(i).or_default();
                post_card(ui, i, anim)
            });

        let root = ui.v_stack(&[title_strip, list])
            .gap(0.0)
            .cross(CrossAxis::Stretch)
            .build();
        ui.present(root);
    }
}

/// Build one post card. The LazyColumn hands every item a slot at
/// `(viewport_w, CARD_ITEM_H)`. We centre a `CARD_W`-wide column
/// inside the slot using a horizontal stack with `MainAxis::Center`.
///
/// Layout (top-down):
///   - Header (56 dp): circular avatar + username + meta + "•••"
///   - Photo pager (PHOTO_SIZE × PHOTO_SIZE): LazyRow + SnapMode::Page
///   - Action row (44 dp): heart, comment, share buttons
///   - Caption (56 dp): like count + username (bold) + caption text
fn post_card(ui: &mut Ui<'_, '_, '_>, i: u32, anim: CardAnim) -> NodeId {
    let header = build_header(ui, i);
    let photo_pager = build_photo_pager(ui, i);
    let action_row = build_action_row(ui, i, &anim);
    let caption = build_caption(ui, i, &anim);

    let inner = ui.v_stack(&[header, photo_pager, action_row, caption])
        .gap(0.0)
        .cross(CrossAxis::Stretch)
        .build();

    let bg = ui.rect(CARD_BG).radius(14.0).build();
    let card = ui.frame(&[bg, inner])
        .size(CARD_W, CARD_ITEM_H)
        .clip_radius(14.0)
        .build();

    // Centre the card in the LazyColumn's full-width slot. The
    // outer h_stack uses MainAxis::Center, no grow on the card,
    // so the slot's extra width becomes side gutters automatically.
    ui.h_stack(&[card])
        .main(MainAxis::Center)
        .cross(CrossAxis::Stretch)
        .build()
}

fn build_header(ui: &mut Ui<'_, '_, '_>, i: u32) -> NodeId {
    // Per-card avatar colour cycles through a small palette so cards
    // are visually distinct without needing real bitmaps.
    const AVATAR_PALETTE: [u32; 6] = [
        0xff2256d3, 0xfff05d4a, 0xff15b97a,
        0xff8c5cf6, 0xfff15ba0, 0xff14b87b,
    ];
    let avatar_color = AVATAR_PALETTE[i as usize % AVATAR_PALETTE.len()];

    // Avatar: 40×40 coloured Rect inside a 40×40 Frame with
    // clip_radius = 20 → perfect circle. A 4-dp ring frame around
    // it gives the Instagram-y "story" look.
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

fn build_photo_pager(ui: &mut Ui<'_, '_, '_>, i: u32) -> NodeId {
    // Each card gets its own scroll-state key so the pager state
    // survives LazyColumn churn AND doesn't leak across cards. The
    // SDK's HashMap<key, ScrollCell> takes care of persistence.
    let key = format!("post-{i}-photos");
    ui.lazy_row(PHOTOS_PER_CARD)
        .key(&key)
        .item_w(PHOTO_SIZE)
        .gap(0.0)
        .size(CARD_W, PHOTO_SIZE)
        .snap(SnapMode::Page)
        .items(move |ui, j| photo_block(ui, i, j))
}

/// Build one synthetic photo "block". No real bitmap — we just
/// arrange a few coloured rounded Rects to make each (card, photo)
/// look distinct. Deterministic from (i, j) so e2e tests can
/// reason about colours without snapshotting bytes.
fn photo_block(ui: &mut Ui<'_, '_, '_>, card: u32, photo: u32) -> NodeId {
    // 12-colour palette indexed by a small per-card×per-photo hash.
    // Even-photo cards use one half of the palette, odd cards the
    // other, so neighbouring photos always contrast.
    const PALETTE: [u32; 12] = [
        0xff1f3a8a, 0xfff97316, 0xff047857, 0xffb91c1c,
        0xff7c3aed, 0xff0891b2, 0xffdb2777, 0xff15803d,
        0xfff59e0b, 0xff4f46e5, 0xff334155, 0xffe11d48,
    ];
    let h = (card.wrapping_mul(7).wrapping_add(photo.wrapping_mul(3))) as usize;
    let bg_color = PALETTE[h % PALETTE.len()];
    let accent_color = PALETTE[(h + 5) % PALETTE.len()];

    let bg = ui.rect(bg_color).size(PHOTO_SIZE, PHOTO_SIZE).build();
    // Two rotated-looking accent rects via offset padding. Cheap
    // "photo composition" stand-in: a big circle in the bottom-
    // right plus a thin band across the top.
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
    // Heart icon: solid red glyph when liked, hollow grey otherwise.
    // Stone 3.6 wraps this Frame in a `push_transform(centred, scale)`
    // so the spring animation pops the heart on tap.
    let heart_label = if anim.liked { "♥" } else { "♡" };
    let heart_color = if anim.liked { 0xffe11d48 } else { TEXT_FG };
    let heart = ui.text(heart_label).size(22.0).color(heart_color).build();
    let heart_btn_bg = ui.rect(0x00_00_00_00).size(32.0, 32.0).build();
    let heart_btn = ui.frame(&[heart_btn_bg, heart])
        .size(32.0, 32.0)
        .build();

    // Comment + share buttons — plain text glyphs for now.
    let comment = ui.text("💬").size(20.0).color(TEXT_FG).build();
    let share = ui.text("↗").size(22.0).color(TEXT_FG).build();

    // Real `ui.button` instances so `was_clicked("like-{i}")` can
    // dispatch the like toggle in stone 3.6.
    let _like_id = format!("like-{i}");
    let _comment_id = format!("comment-{i}");
    let _share_id = format!("share-{i}");

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

    // Sample captions cycled by index — long enough to test the
    // 2-line ellipsis path on most cards.
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
