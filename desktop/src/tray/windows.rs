//! Windows tray icon. Runs on its own thread with a message loop, which the
//! tray icon needs; status changes are posted to that thread.

use std::{mem, path::PathBuf, ptr, sync::mpsc as std_mpsc};

use tokio::sync::{mpsc, watch};
use tray_icon::{
    Icon, TrayIcon, TrayIconBuilder,
    menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu},
};
use windows_sys::Win32::{
    System::Threading::GetCurrentThreadId,
    UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW,
        TranslateMessage, WM_APP,
    },
};

use super::{MENU_TITLE_SIDE, SIDES, app_icon_rgba, open_with};
use crate::{
    config::Paths,
    engine::{Command, Status},
    log,
};

const WM_STATUS_CHANGED: u32 = WM_APP + 1;

/// Shows the tray icon and keeps it in step with `status`.
pub fn spawn(
    mut status: watch::Receiver<Status>,
    commands: mpsc::UnboundedSender<Command>,
    paths: &Paths,
) {
    let (config_file, log_file) = (paths.config_file.clone(), paths.log_file.clone());
    let thread_status = status.clone();
    let (started, thread_id) = std_mpsc::channel();
    std::thread::spawn(move || {
        if let Err(e) = run(thread_status, commands, config_file, log_file, started) {
            log(format!("no tray icon: {e}"));
        }
    });
    let Ok(thread_id) = thread_id.recv() else {
        return;
    };
    tokio::spawn(async move {
        while status.changed().await.is_ok() {
            unsafe { PostThreadMessageW(thread_id, WM_STATUS_CHANGED, 0, 0) };
        }
    });
}

fn run(
    mut status: watch::Receiver<Status>,
    commands: mpsc::UnboundedSender<Command>,
    config_file: PathBuf,
    log_file: PathBuf,
    started: std_mpsc::Sender<u32>,
) -> Result<(), String> {
    let mut message: MSG = unsafe { mem::zeroed() };
    // Creates this thread's message queue, so status posts can't get lost.
    unsafe { PeekMessageW(&mut message, ptr::null_mut(), 0, 0, PM_NOREMOVE) };
    let _ = started.send(unsafe { GetCurrentThreadId() });

    let (rgba, width, height) = app_icon_rgba()?;
    let icon = Icon::from_rgba(rgba, width, height).map_err(|e| e.to_string())?;
    let current = status.borrow_and_update().clone();
    let tray = TrayIconBuilder::new()
        .with_icon(icon)
        .with_tooltip(tooltip(&current))
        .with_menu(Box::new(menu(&current)?))
        .with_menu_on_left_click(true)
        .build()
        .map_err(|e| e.to_string())?;

    let menu_status = status.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let command = match event.id.as_ref() {
            "enabled" => Command::SetEnabled(!menu_status.borrow().enabled),
            "settings" => return open_with("notepad.exe", &config_file),
            "log" => return open_with("notepad.exe", &log_file),
            "reload" => Command::ReloadConfig,
            "quit" => Command::Quit,
            id => match SIDES.iter().find(|(_, label)| id == side_id(label)) {
                Some((side, _)) => Command::SetSide(*side),
                None => return,
            },
        };
        let _ = commands.send(command); // only fails once the engine has stopped
    }));

    while unsafe { GetMessageW(&mut message, ptr::null_mut(), 0, 0) } > 0 {
        if message.hwnd.is_null() && message.message == WM_STATUS_CHANGED {
            update(&tray, &status.borrow_and_update());
            continue;
        }
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    Ok(())
}

fn update(tray: &TrayIcon, status: &Status) {
    if let Err(e) = tray.set_tooltip(Some(tooltip(status))) {
        log(format!("tray tooltip: {e}"));
    }
    match menu(status) {
        Ok(menu) => tray.set_menu(Some(Box::new(menu))),
        Err(e) => log(format!("tray menu: {e}")),
    }
}

/// Windows cuts tray tooltips at 127 characters.
fn tooltip(status: &Status) -> String {
    let text = format!("Tablet Edge\n{}", status.lines().join("\n"));
    text.chars().take(127).collect()
}

fn side_id(label: &str) -> String {
    format!("side-{label}")
}

fn menu(status: &Status) -> Result<Menu, String> {
    let menu = Menu::new();
    let error = |e: tray_icon::menu::Error| e.to_string();
    for line in status.lines() {
        menu.append(&MenuItem::new(line.replace('&', "&&"), false, None))
            .map_err(error)?;
    }
    let sides = Submenu::new(MENU_TITLE_SIDE, true);
    for (side, label) in SIDES {
        let item = CheckMenuItem::with_id(
            side_id(label),
            format!("{label} edge"),
            true,
            side == status.side,
            None,
        );
        sides.append(&item).map_err(error)?;
    }
    menu.append_items(&[
        &PredefinedMenuItem::separator(),
        &CheckMenuItem::with_id("enabled", "Enabled", true, status.enabled, None),
        &sides,
        &PredefinedMenuItem::separator(),
        &MenuItem::with_id("settings", "Edit settings file", true, None),
        &MenuItem::with_id("reload", "Reload settings", true, None),
        &MenuItem::with_id("log", "Show log", true, None),
        &PredefinedMenuItem::separator(),
        &MenuItem::with_id(MenuId::new("quit"), "Quit", true, None),
    ])
    .map_err(error)?;
    Ok(menu)
}
