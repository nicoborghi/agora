//! The single tray icon: the app icon as raw 32×32 RGBA (`icons/tray-32.rgba`, made by
//! `icons/make-icons.ps1` with the other icons), with the unread dot drawn on top in code, so no image crate is needed.
//! The taskbar overlay dots, and focus mode's "zzz", are drawn in code too.

use tauri::image::Image;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

const TRAY_ID: &str = "agora";
const SIZE: usize = 32;
/// The app icon at tray size, straight RGBA (not premultiplied).
const ICON: &[u8; SIZE * SIZE * 4] = include_bytes!("../icons/tray-32.rgba");
const RING: [u8; 3] = [0x16, 0x17, 0x1a];
/// The general "something is unread" dot on the tray icon.
const ALERT: [u8; 3] = [0xe5, 0x48, 0x4d];
/// Focus mode's "zzz" on the taskbar: white letters with a dark edge, for light and dark taskbars.
const WHITE: [u8; 3] = [0xff, 0xff, 0xff];

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Open Agora", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(render(&[], false))
        .tooltip("Agora")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => show_main(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                show_main(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// The tray shows one general dot when anything is unread; the taskbar button shows
/// one dot per workspace, in its colour. In focus mode both show "zzz" instead.
pub fn set_dots(app: &AppHandle, colors: &[String], focus: bool) {
    let rgb: Vec<[u8; 3]> = colors.iter().filter_map(|c| parse_hex(c)).collect();
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let general: &[[u8; 3]] = if rgb.is_empty() { &[] } else { &[ALERT] };
        let _ = tray.set_icon(Some(render(general, focus)));
        let tip = match (focus, rgb.is_empty()) {
            (true, _) => "Agora: focus mode, notifications off",
            (false, true) => "Agora",
            (false, false) => "Agora: unread messages",
        };
        let _ = tray.set_tooltip(Some(tip));
    }
    if let Some(w) = app.get_webview_window("main") {
        // Windows: a dot per workspace on the taskbar button. macOS: a dot badge on the Dock icon.
        // Linux: the launcher badge (where the desktop supports it) counts workspaces with news.
        #[cfg(windows)]
        let _ = w.set_overlay_icon(match (focus, rgb.is_empty()) {
            (true, _) => Some(render_sleep()),
            (false, true) => None,
            (false, false) => Some(render_overlay(&rgb)),
        });
        #[cfg(target_os = "macos")]
        let _ = w.set_badge_label(match (focus, rgb.is_empty()) {
            (true, _) => Some("zzz".to_string()),
            (false, true) => None,
            (false, false) => Some("•".to_string()),
        });
        #[cfg(target_os = "linux")]
        let _ = w.set_badge_count(if rgb.is_empty() || focus { None } else { Some(rgb.len() as i64) });
    }
}

/// Focus mode on the taskbar button: three z's, rising and shrinking towards the top right.
#[cfg(windows)]
fn render_sleep() -> Image<'static> {
    let mut px = vec![0u8; SIZE * SIZE * 4];
    // (left, top, width, height, stroke) of each z.
    let zs = [(1.5, 18.0, 10.0, 12.0, 2.8), (14.0, 6.0, 6.5, 8.0, 2.3), (24.0, 0.8, 5.0, 4.0, 1.8)];
    // The dark edge first, under all three, then the white letters.
    for (color, extra) in [(RING, 2.0), (WHITE, 0.0)] {
        for (x, y, zw, zh, w) in zs {
            let w = w + extra;
            line(&mut px, (x, y), (x + zw, y), w, color);
            line(&mut px, (x + zw, y), (x, y + zh), w, color);
            line(&mut px, (x, y + zh), (x + zw, y + zh), w, color);
        }
    }
    Image::new_owned(px, SIZE as u32, SIZE as u32)
}

/// An antialiased stroke from `a` to `b`, `w` pixels wide.
fn line(px: &mut [u8], a: (f32, f32), b: (f32, f32), w: f32, color: [u8; 3]) {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = (dx * dx + dy * dy).max(1e-6);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let t = (((fx - a.0) * dx + (fy - a.1) * dy) / len2).clamp(0.0, 1.0);
            let d = ((fx - a.0 - t * dx).powi(2) + (fy - a.1 - t * dy).powi(2)).sqrt();
            blend(px, x, y, color, (w / 2.0 - d + 0.5).clamp(0.0, 1.0));
        }
    }
}

/// Taskbar overlay (bottom-right of the button): 1 dot, 2 side by side, 3–4 in a 2×2 grid.
#[cfg(windows)]
fn render_overlay(dots: &[[u8; 3]]) -> Image<'static> {
    let mut px = vec![0u8; SIZE * SIZE * 4];
    let (r, centers): (f32, &[(f32, f32)]) = match dots.len() {
        1 => (13.0, &[(16.0, 16.0)][..]),
        2 => (8.5, &[(8.5, 16.0), (23.5, 16.0)][..]),
        _ => (7.5, &[(8.5, 8.5), (23.5, 8.5), (8.5, 23.5), (23.5, 23.5)][..]),
    };
    for (color, (cx, cy)) in dots.iter().zip(centers) {
        disc(&mut px, *cx, *cy, r, *color);
    }
    Image::new_owned(px, SIZE as u32, SIZE as u32)
}

/// A dot with a dark ring, so it stands out on light and dark taskbars.
fn disc(px: &mut [u8], cx: f32, cy: f32, r: f32, color: [u8; 3]) {
    for y in 0..SIZE {
        for x in 0..SIZE {
            let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
            blend(px, x, y, RING, (r - d + 0.5).clamp(0.0, 1.0));
            blend(px, x, y, color, (r - 2.0 - d + 0.5).clamp(0.0, 1.0));
        }
    }
}

fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let p = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).ok();
    Some([p(0)?, p(2)?, p(4)?])
}

/// The app icon, plus one larger unread dot in the top-right corner when `dots` is not empty.
/// In focus mode the icon is grayed out, with no dot.
fn render(dots: &[[u8; 3]], focus: bool) -> Image<'static> {
    let mut px = ICON.to_vec();
    if focus {
        for p in px.chunks_exact_mut(4) {
            let gray = (0.3 * p[0] as f32 + 0.59 * p[1] as f32 + 0.11 * p[2] as f32).round() as u8;
            p[..3].fill(gray);
            p[3] = (p[3] as f32 * 0.8).round() as u8;
        }
    } else if let Some(color) = dots.first() {
        disc(&mut px, 24.0, 8.0, 8.0, *color);
    }
    Image::new_owned(px, SIZE as u32, SIZE as u32)
}

fn blend(px: &mut [u8], x: usize, y: usize, rgb: [u8; 3], a: f32) {
    if a <= 0.0 {
        return;
    }
    let i = (y * SIZE + x) * 4;
    let da = px[i + 3] as f32 / 255.0;
    let out_a = a + da * (1.0 - a);
    for c in 0..3 {
        let src = rgb[c] as f32;
        let dst = px[i + c] as f32;
        px[i + c] = ((src * a + dst * da * (1.0 - a)) / out_a.max(1e-6)).round() as u8;
    }
    px[i + 3] = (out_a * 255.0).round() as u8;
}
