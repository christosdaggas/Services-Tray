use crate::model::{Action, Controller, Status, APP_NAME};
use ksni::menu::{MenuItem, StandardItem, SubMenu};
use ksni::{Icon, ToolTip};
use std::io::Cursor;
use std::sync::Arc;

pub type Handle = ksni::blocking::Handle<ServicesTray>;

/// D-Bus name of the tray host's registry, which the icon registers with.
pub const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";

/// Creates the tray icon; call it once `WATCHER_NAME` is on the bus. Should
/// the host vanish before registration, ksni waits for it to come back.
pub fn spawn(controller: Arc<Controller>) -> Result<Handle, ksni::Error> {
    use ksni::blocking::TrayMethods;
    ServicesTray::new(controller)
        .assume_sni_available(true)
        .spawn()
}

pub struct ServicesTray {
    pub controller: Arc<Controller>,
    icon: Icon,
}

impl ServicesTray {
    pub fn new(controller: Arc<Controller>) -> Self {
        Self {
            controller,
            icon: fallback_icon(),
        }
    }

    fn snapshot(&self) -> (Vec<(String, Status)>, Status) {
        let state = self.controller.state.lock().unwrap();
        let services = state
            .services
            .iter()
            .map(|service| {
                let status = state
                    .statuses
                    .get(&service.key())
                    .copied()
                    .unwrap_or(Status::Unknown);
                (service.display_name(), status)
            })
            .collect::<Vec<_>>();
        let overall = if services.iter().any(|(_, status)| *status == Status::Failed) {
            Status::Failed
        } else {
            Status::Active
        };
        (services, overall)
    }
}

impl ksni::Tray for ServicesTray {
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "services-tray".into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::ApplicationStatus
    }

    fn title(&self) -> String {
        let (services, _) = self.snapshot();
        let running = services.iter().filter(|(_, s)| s.is_active()).count();
        format!("{APP_NAME} — {running}/{} running", services.len())
    }

    fn status(&self) -> ksni::Status {
        let (_, overall) = self.snapshot();
        if overall == Status::Failed {
            ksni::Status::NeedsAttention
        } else {
            ksni::Status::Active
        }
    }

    fn icon_name(&self) -> String {
        String::new()
    }

    fn attention_icon_name(&self) -> String {
        String::new()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        vec![self.icon.clone()]
    }

    fn attention_icon_pixmap(&self) -> Vec<Icon> {
        vec![self.icon.clone()]
    }

    fn tool_tip(&self) -> ToolTip {
        let (services, _) = self.snapshot();
        let description = if services.is_empty() {
            "No services added yet".to_owned()
        } else {
            services
                .iter()
                .map(|(name, status)| format!("{name} — {}", status.label()))
                .collect::<Vec<_>>()
                .join("\n")
        };
        ToolTip {
            icon_name: String::new(),
            icon_pixmap: vec![self.icon.clone()],
            title: APP_NAME.into(),
            description,
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let (services, busy) = {
            let state = self.controller.state.lock().unwrap();
            let services = state
                .services
                .iter()
                .map(|service| {
                    let status = state
                        .statuses
                        .get(&service.key())
                        .copied()
                        .unwrap_or(Status::Unknown);
                    (service.clone(), status)
                })
                .collect::<Vec<_>>();
            (services, state.busy.clone())
        };

        let mut items: Vec<MenuItem<Self>> = Vec::new();
        if services.is_empty() {
            items.push(
                StandardItem {
                    label: "No services added yet".into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        }

        for (service, status) in services {
            let working = busy.contains(&service.key());
            let label = padded_menu_label(&format!(
                "{} {}",
                status_dot(status),
                escape_menu(&service.display_name())
            ));
            let mut actions: Vec<MenuItem<Self>> = Vec::new();

            let target = service.clone();
            actions.push(
                StandardItem {
                    label: "▶  _Start".into(),
                    enabled: !working && status.can_start(),
                    activate: Box::new(move |tray: &mut Self| {
                        tray.controller.action(target.clone(), Action::Start);
                    }),
                    ..Default::default()
                }
                .into(),
            );
            let target = service.clone();
            actions.push(
                StandardItem {
                    label: "↻  _Restart".into(),
                    enabled: !working && status.can_restart(),
                    activate: Box::new(move |tray: &mut Self| {
                        tray.controller.action(target.clone(), Action::Restart);
                    }),
                    ..Default::default()
                }
                .into(),
            );
            let target = service.clone();
            actions.push(
                StandardItem {
                    label: "■  S_top".into(),
                    enabled: !working && status.can_stop(),
                    activate: Box::new(move |tray: &mut Self| {
                        tray.controller.action(target.clone(), Action::Stop);
                    }),
                    ..Default::default()
                }
                .into(),
            );

            if !service.links.is_empty() {
                actions.push(MenuItem::Separator);
            }
            for link in &service.links {
                let target = link.clone();
                actions.push(
                    StandardItem {
                        label: format!("↗  {}", escape_menu(link.label())),
                        activate: Box::new(move |tray: &mut Self| {
                            tray.controller.request_open(target.clone());
                        }),
                        ..Default::default()
                    }
                    .into(),
                );
            }

            items.push(
                SubMenu {
                    label,
                    submenu: actions,
                    ..Default::default()
                }
                .into(),
            );
        }

        items.extend([
            MenuItem::Separator,
            StandardItem {
                label: "A_dd Service…".into(),
                activate: Box::new(|tray: &mut Self| tray.controller.request_add()),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "_Manage Services…".into(),
                activate: Box::new(|tray: &mut Self| tray.controller.request_manage()),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: autostart_menu_label(crate::autostart::is_enabled()),
                activate: Box::new(|tray: &mut Self| {
                    let enable = !crate::autostart::is_enabled();
                    let controller = tray.controller.clone();
                    std::thread::spawn(move || {
                        if let Err(error) = crate::autostart::set_enabled(enable) {
                            crate::notify_error(
                                "Could not change tray autostart",
                                &format!("{error:#}"),
                            );
                        }
                        controller.refresh_tray();
                    });
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "_Quit Services Tray".into(),
                activate: Box::new(|tray: &mut Self| tray.controller.quit()),
                ..Default::default()
            }
            .into(),
        ]);
        items
    }
}

fn escape_menu(label: &str) -> String {
    label.replace('_', "__")
}

fn padded_menu_label(label: &str) -> String {
    const WIDTH: usize = 22;
    let visible = label.replace("__", "_").chars().count();
    format!(
        "{label}{}",
        "\u{2800}".repeat(WIDTH.saturating_sub(visible))
    )
}

fn autostart_menu_label(enabled: bool) -> String {
    if enabled {
        "Start tray on _login  \u{2714}".to_owned()
    } else {
        "Start tray on _login".to_owned()
    }
}

fn status_dot(status: Status) -> &'static str {
    match status {
        Status::Active => "🟢",
        Status::Activating | Status::Deactivating => "🟠",
        Status::Inactive | Status::Failed | Status::NotFound => "🔴",
        Status::Unknown => "⚪",
    }
}

fn fallback_icon() -> Icon {
    let foreground = if prefers_dark_theme() { 255 } else { 0 };
    fallback_icon_with_foreground(foreground)
}

fn fallback_icon_with_foreground(foreground: u8) -> Icon {
    let decoder = png::Decoder::new(Cursor::new(include_bytes!(
        "../assets/services-tray-48.png"
    )));
    let mut reader = decoder.read_info().expect("embedded tray icon is valid");
    let mut rgba = vec![0; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut rgba)
        .expect("embedded tray icon can be decoded");
    rgba.truncate(info.buffer_size());

    let mut data = Vec::with_capacity(rgba.len());
    let (pixels, remainder) = rgba.as_chunks::<4>();
    debug_assert!(remainder.is_empty());
    for pixel in pixels {
        let alpha = pixel[3];
        data.extend_from_slice(&[alpha, foreground, foreground, foreground]);
    }
    Icon {
        width: info.width as i32,
        height: info.height as i32,
        data,
    }
}

fn prefers_dark_theme() -> bool {
    std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "color-scheme"])
        .output()
        .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).contains("prefer-dark"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_has_valid_argb_shape() {
        let icon = fallback_icon_with_foreground(255);
        assert_eq!(icon.width, 48);
        assert_eq!(icon.height, 48);
        assert_eq!(icon.data.len(), 48 * 48 * 4);
    }

    #[test]
    fn menu_labels_escape_underscores() {
        assert_eq!(escape_menu("my_service"), "my__service");
    }

    #[test]
    fn padding_counts_an_escaped_underscore_once() {
        let padded = padded_menu_label(&escape_menu("my_service"));
        assert_eq!(padded.chars().filter(|c| *c == '\u{2800}').count(), 22 - 10);
    }

    #[test]
    fn service_menu_lists_links_after_the_actions() {
        use crate::model::{Scope, ServiceConfig, ServiceLink, SharedState};

        let mut service = ServiceConfig::new("httpd", Scope::System, Some("Apache"));
        let link = ServiceLink::new("Home_Page", "127.0.0.1:80").unwrap();
        service.links = vec![link.clone()];
        let state = SharedState::default();
        state.lock().unwrap().services = vec![service];
        let mut tray = ServicesTray::new(Arc::new(Controller::new(state.clone())));

        let menu = ksni::Tray::menu(&tray);
        let Some(MenuItem::SubMenu(submenu)) = menu.into_iter().next() else {
            panic!("the first menu item should be the service submenu");
        };
        let labels: Vec<String> = submenu
            .submenu
            .iter()
            .map(|item| match item {
                MenuItem::Standard(item) => item.label.clone(),
                MenuItem::Separator => "---".into(),
                _ => "?".into(),
            })
            .collect();
        assert_eq!(
            labels,
            [
                "▶  _Start",
                "↻  _Restart",
                "■  S_top",
                "---",
                "↗  Home__Page"
            ]
        );

        let Some(MenuItem::Standard(open)) = submenu.submenu.last() else {
            panic!("the last item should open the link");
        };
        (open.activate)(&mut tray);
        assert_eq!(state.lock().unwrap().open_requested, [link]);
    }
}
