//! Keep indexing after the window is closed.
//!
//! Closing the window while documents are being indexed hides it instead: the app keeps
//! working from the notification area at lower CPU priority, says so once, and quits by
//! itself when the last index job is done (or when "ออก" is picked from the tray menu —
//! unfinished files stay queued and continue on the next start).

use crate::commands::AppState;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, WindowEvent};
use tauri_plugin_notification::NotificationExt;

const TRAY_ID: &str = "docask";
static HIDDEN: AtomicBool = AtomicBool::new(false);

/// Create the (hidden) tray icon. Called once from `setup`.
pub fn init(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "เปิด DocAsk", true, None::<&str>)?;
    let stop = MenuItem::with_id(app, "quit", "หยุดทำ index และออก", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &stop])?;
    let mut tray = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("DocAsk — กำลังทำ index เบื้องหลัง")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, e| match e.id().as_ref() {
            "open" => show(app),
            "quit" => quit(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, e| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                show(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    let tray: TrayIcon = tray.build(app)?;
    tray.set_visible(false)?;
    Ok(())
}

/// Window events: hide instead of close while indexing.
pub fn on_window_event(window: &tauri::Window, event: &WindowEvent) {
    let WindowEvent::CloseRequested { api, .. } = event else { return };
    let app = window.app_handle();
    if !app.state::<AppState>().indexer.any_running() {
        return; // nothing to finish → close normally
    }
    api.prevent_close();
    let _ = window.hide();
    HIDDEN.store(true, Ordering::SeqCst);
    set_low_priority(true);
    if let Some(t) = app.tray_by_id(TRAY_ID) {
        let _ = t.set_visible(true);
    }
    notify(app, "DocAsk ทำ index ต่อเบื้องหลัง", "จะปิดเองเมื่อเสร็จ · คลิกไอคอนที่ถาดข้างนาฬิกาเพื่อเปิดกลับ");
}

/// Called by the indexer when no job is left.
pub fn on_indexing_idle(app: &AppHandle) {
    if HIDDEN.load(Ordering::SeqCst) {
        notify(app, "DocAsk ทำ index เสร็จแล้ว", "เอกสารพร้อมให้ถามแล้ว");
        // give the notification a moment to be handed to Windows
        let app = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(800));
            if HIDDEN.load(Ordering::SeqCst) {
                app.exit(0);
            }
        });
    }
}

fn show(app: &AppHandle) {
    HIDDEN.store(false, Ordering::SeqCst);
    set_low_priority(false);
    if let Some(t) = app.tray_by_id(TRAY_ID) {
        let _ = t.set_visible(false);
    }
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn quit(app: &AppHandle) {
    app.state::<AppState>().indexer.cancel_all();
    // the index threads see the flag between batches; files not finished stay queued
    let app = app.clone();
    std::thread::spawn(move || {
        let st = app.state::<AppState>();
        for _ in 0..50 {
            if !st.indexer.any_running() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        app.exit(0);
    });
}

fn notify(app: &AppHandle, title: &str, body: &str) {
    if let Err(e) = app.notification().builder().title(title).body(body).show() {
        log::warn!("notification failed: {e}");
    }
}

/// Below-normal priority: Windows gives the CPU to whatever the user is doing first.
#[cfg(windows)]
fn set_low_priority(low: bool) {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> isize;
        fn SetPriorityClass(process: isize, class: u32) -> i32;
    }
    const NORMAL_PRIORITY_CLASS: u32 = 0x20;
    const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x4000;
    unsafe {
        SetPriorityClass(GetCurrentProcess(), if low { BELOW_NORMAL_PRIORITY_CLASS } else { NORMAL_PRIORITY_CLASS });
    }
}

#[cfg(not(windows))]
fn set_low_priority(_low: bool) {}
