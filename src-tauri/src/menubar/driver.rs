//! The thread that keeps the menu bar current.
//!
//! The icon is static: the same reading is the same reading until the numbers
//! move. So this loop spends almost all its time asleep and redraws only when
//! the picture actually changes — a gauge that repaints on a timer is a
//! battery drain with no readership, and the spec rules animation out
//! entirely.

use std::sync::Arc;
use std::time::Duration;

use tauri::image::Image;
use tauri::{AppHandle, Runtime};

use super::{Menubar, ProviderView};
use crate::art;
use crate::credentials::now_ms;
use crate::tray;
use crate::usage::poller::UsageRegistry;

/// How often to look. Nothing here changes faster than the polling interval,
/// so one wake a second is already generous — it costs nothing measurable and
/// keeps the icon within a second of the truth.
const TICK: Duration = Duration::from_secs(1);

pub fn spawn<R: Runtime>(app: AppHandle<R>, registry: Arc<UsageRegistry>) {
    std::thread::Builder::new()
        .name("ai-reactor-menubar".into())
        .spawn(move || run(app, registry))
        .expect("failed to spawn the menu bar thread");
}

fn run<R: Runtime>(app: AppHandle<R>, registry: Arc<UsageRegistry>) {
    let mut menubar = Menubar::new();
    let mut drawn: Option<Vec<ProviderView>> = None;

    loop {
        let snapshots = registry.snapshots();
        let now = now_ms();
        let tick = menubar.update(&snapshots);

        // Comparing the whole set of views rather than tracking per-provider
        // dirty flags: the views *are* the picture, so if none of them
        // changed there is nothing to redraw.
        if drawn.as_ref() != Some(&tick.views) {
            draw(&app, &tick.views);
            drawn = Some(tick.views);
        }

        if let Some((provider, window)) = tick.notify {
            eprintln!(
                "[ai-reactor] 알림: {} {} 한도 {:.0}% 남음",
                provider.label(),
                window.label(),
                window.remaining_percent
            );
            crate::notify::limit_nearly_spent(&app, provider, &window, now);
        }

        std::thread::sleep(TICK);
    }
}

/// One status item, one bitmap — each visible provider's own ring (drawn by
/// `art::render`, unchanged) is stitched in side by side, left to right in
/// `ProviderId::ALL`'s order, rather than each provider owning a separate
/// status item. Two full-size icons read as two separate apps sharing the
/// bar by coincidence; this reads as one thing with two rings in it, which
/// is what was actually asked for.
fn draw<R: Runtime>(app: &AppHandle<R>, views: &[ProviderView]) {
    let Some(icon) = app.tray_by_id(tray::ID) else {
        return;
    };

    let visible: Vec<&ProviderView> = views.iter().filter(|v| v.view.is_some()).collect();

    // Nothing set up here at all: hide the status item entirely, the same
    // quiet invitation the popover gives an absent provider's card, rather
    // than showing an icon for nothing the user uses.
    if visible.is_empty() {
        if let Err(e) = icon.set_visible(false) {
            eprintln!("[ai-reactor] 트레이 숨기기 실패: {e}");
        }
        return;
    }
    if let Err(e) = icon.set_visible(true) {
        eprintln!("[ai-reactor] 트레이 표시 실패: {e}");
    }

    let renders: Vec<(Vec<u8>, u32, u32)> = visible
        .iter()
        .map(|v| {
            let mark = tray::mark_for(v.provider);
            art::render(v.view.unwrap().percent, mark)
        })
        .collect();
    let (pixels, width, height) = stitch_horizontally(&renders, GAUGE_GAP_PX);

    // A template image: alpha only, tinted by macOS to match the bar. Dark
    // and light modes are handled for us.
    let image = Image::new(&pixels, width, height);
    if let Err(e) = icon.set_icon_with_as_template(Some(image), true) {
        eprintln!("[ai-reactor] 트레이 아이콘 갱신 실패: {e}");
    }
    // The bitmap is drawn at 2×, so its point size is half its pixel size.
    crate::platform::set_tray_image_size(&icon, width as f64 / 2.0, height as f64 / 2.0);

    // The number is inside each gauge now, under its logo, so the status
    // item carries no text of its own.
    if let Err(e) = icon.set_title(None::<&str>) {
        eprintln!("[ai-reactor] 트레이 제목 갱신 실패: {e}");
    }
}

/// Transparent space between two gauges, in pixels (so 3pt): enough that
/// two rings read as two gauges in one group, not one blurred shape.
const GAUGE_GAP_PX: u32 = 6;

/// Lay out same-height RGBA buffers side by side into one wider image, with
/// `gap` transparent pixels between neighbours.
fn stitch_horizontally(images: &[(Vec<u8>, u32, u32)], gap: u32) -> (Vec<u8>, u32, u32) {
    let height = images[0].2;
    let total_width: u32 =
        images.iter().map(|(_, w, _)| w).sum::<u32>() + gap * (images.len() as u32 - 1);
    let mut out = vec![0u8; (total_width * height * 4) as usize];

    let mut x_offset = 0u32;
    for (pixels, w, h) in images {
        debug_assert_eq!(*h, height, "stitched icons must share a height");
        for y in 0..height {
            let src_row = (y * w * 4) as usize;
            let dst_row = (y * total_width * 4 + x_offset * 4) as usize;
            let row_bytes = (*w * 4) as usize;
            out[dst_row..dst_row + row_bytes].copy_from_slice(&pixels[src_row..src_row + row_bytes]);
        }
        x_offset += w + gap;
    }

    (out, total_width, height)
}
