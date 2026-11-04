//! Linux tray icon (StatusNotifierItem over D-Bus, no GTK). GNOME shows it
//! with the AppIndicator extension, which Ubuntu enables by default.

use std::{path::PathBuf, sync::LazyLock};

use ksni::{
    Icon, MenuItem, TrayMethods,
    menu::{CheckmarkItem, RadioGroup, RadioItem, StandardItem, SubMenu},
};
use tokio::sync::{mpsc, watch};

use super::{MENU_TITLE_SIDE, SIDES, app_icon_rgba, open_with};
use crate::{
    config::Paths,
    engine::{Command, Status},
    log,
};

struct Tray {
    status: Status,
    commands: mpsc::UnboundedSender<Command>,
    config_file: PathBuf,
    log_file: PathBuf,
}

impl Tray {
    fn send(&self, command: Command) {
        // Only fails once the engine has stopped, when there is nothing left to tell.
        let _ = self.commands.send(command);
    }
}

impl ksni::Tray for Tray {
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "droidleap".into()
    }

    fn title(&self) -> String {
        "DroidLeap".into()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        ICON.iter().cloned().collect()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "DroidLeap".into(),
            description: self.status.lines().join("\n"),
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut items: Vec<MenuItem<Self>> = self
            .status
            .lines()
            .into_iter()
            .map(|label| {
                StandardItem {
                    label,
                    enabled: false,
                    ..Default::default()
                }
                .into()
            })
            .collect();
        items.extend([
            MenuItem::Separator,
            CheckmarkItem {
                label: "Enabled".into(),
                checked: self.status.enabled,
                activate: Box::new(|tray: &mut Self| {
                    tray.send(Command::SetEnabled(!tray.status.enabled))
                }),
                ..Default::default()
            }
            .into(),
            SubMenu {
                label: MENU_TITLE_SIDE.into(),
                submenu: vec![
                    RadioGroup {
                        selected: SIDES
                            .iter()
                            .position(|(side, _)| *side == self.status.side)
                            .unwrap_or(0),
                        select: Box::new(|tray: &mut Self, index| {
                            tray.send(Command::SetSide(SIDES[index].0))
                        }),
                        options: SIDES
                            .iter()
                            .map(|(_, label)| RadioItem {
                                label: format!("{label} edge"),
                                ..Default::default()
                            })
                            .collect(),
                    }
                    .into(),
                ],
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Edit settings file".into(),
                activate: Box::new(|tray: &mut Self| open_with("xdg-open", &tray.config_file)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Reload settings".into(),
                activate: Box::new(|tray: &mut Self| tray.send(Command::ReloadConfig)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Show log".into(),
                activate: Box::new(|tray: &mut Self| open_with("xdg-open", &tray.log_file)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|tray: &mut Self| tray.send(Command::Quit)),
                ..Default::default()
            }
            .into(),
        ]);
        items
    }
}

/// Shows the tray icon and keeps it in step with `status`. Without a tray host
/// the app keeps running; it's then controlled through the settings file.
pub fn spawn(
    mut status: watch::Receiver<Status>,
    commands: mpsc::UnboundedSender<Command>,
    paths: &Paths,
) {
    let tray = Tray {
        status: status.borrow().clone(),
        commands,
        config_file: paths.config_file.clone(),
        log_file: paths.log_file.clone(),
    };
    tokio::spawn(async move {
        let handle = match tray.spawn().await {
            Ok(handle) => handle,
            Err(e) => {
                log(format!(
                    "no tray icon (on GNOME, enable the AppIndicator extension): {e}"
                ));
                return;
            }
        };
        while status.changed().await.is_ok() {
            let latest = status.borrow_and_update().clone();
            handle.update(|tray| tray.status = latest).await;
        }
    });
}

/// The app icon in the ARGB32 layout the tray protocol wants.
static ICON: LazyLock<Option<Icon>> = LazyLock::new(|| match app_icon_rgba() {
    Ok((mut data, width, height)) => {
        for pixel in data.chunks_exact_mut(4) {
            pixel.rotate_right(1); // RGBA to ARGB
        }
        Some(Icon {
            width: width as i32,
            height: height as i32,
            data,
        })
    }
    Err(e) => {
        log(format!("tray icon unusable: {e}"));
        None
    }
});
