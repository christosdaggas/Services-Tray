use crate::model::{
    count_services, normalize_url, read_import, Controller, Scope, ServiceConfig, ServiceLink,
};
use gtk4 as gtk;
use libadwaita as adw;

use adw::prelude::*;
use gtk::{gdk, gio, glib, pango};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

const CSS: &str = r#"
.error-label { color: #e01b24; }
"#;

thread_local! {
    static UI: RefCell<Option<Rc<Ui>>> = const { RefCell::new(None) };
    static STARTED: Cell<bool> = const { Cell::new(false) };
}

pub fn present(app: &adw::Application, controller: Arc<Controller>) {
    let existing = UI.with(|slot| slot.borrow().clone());
    if let Some(ui) = existing {
        ui.open_manage();
        return;
    }

    init_css();
    let ui = Ui::new(app, controller);
    UI.with(|slot| *slot.borrow_mut() = Some(ui.clone()));
    let first_start = !STARTED.with(|started| started.replace(true));
    ui.install();
    if first_start && ui.is_empty() {
        ui.open_manage();
    }
}

fn init_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_data(CSS);
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

struct Ui {
    app: adw::Application,
    controller: Arc<Controller>,
    manage: adw::ApplicationWindow,
    manage_stack: gtk::Stack,
    manage_page: adw::PreferencesPage,
    manage_group: RefCell<Option<adw::PreferencesGroup>>,
    toasts: adw::ToastOverlay,
    export_action: gio::SimpleAction,
    last_services: RefCell<Vec<ServiceConfig>>,
    add_open: Cell<bool>,
}

impl Ui {
    fn new(app: &adw::Application, controller: Arc<Controller>) -> Rc<Self> {
        let manage = adw::ApplicationWindow::builder()
            .application(app)
            .title("Manage Services")
            .default_width(620)
            .default_height(520)
            .build();
        manage.set_icon_name(Some("services-tray"));
        manage.set_hide_on_close(true);

        let manage_header = adw::HeaderBar::new();
        manage_header.set_title_widget(Some(&adw::WindowTitle::new("Manage Services", "")));
        let add_button = gtk::Button::from_icon_name("list-add-symbolic");
        add_button.set_tooltip_text(Some("Add service"));
        add_button.update_property(&[gtk::accessible::Property::Label("Add service")]);
        add_button.add_css_class("flat");
        manage_header.pack_start(&add_button);
        let menu = gio::Menu::new();
        menu.append(Some("_Import Services…"), Some("win.import"));
        menu.append(Some("_Export Services…"), Some("win.export"));
        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&menu)
            .primary(true)
            .tooltip_text("Main menu")
            .build();
        menu_button.update_property(&[gtk::accessible::Property::Label("Main menu")]);
        manage_header.pack_end(&menu_button);

        let manage_stack = gtk::Stack::builder().vexpand(true).build();
        let manage_page = adw::PreferencesPage::new();
        let manage_empty = adw::StatusPage::builder()
            .icon_name("list-add-symbolic")
            .title("No services yet")
            .description("Add a user or system service to control it from the tray menu, or import a list exported earlier.")
            .build();
        let empty_add = gtk::Button::with_label("Add Service");
        empty_add.add_css_class("pill");
        empty_add.add_css_class("suggested-action");
        let empty_import = gtk::Button::builder()
            .label("Import Services…")
            .action_name("win.import")
            .build();
        empty_import.add_css_class("pill");
        let empty_buttons = gtk::Box::new(gtk::Orientation::Vertical, 12);
        empty_buttons.set_halign(gtk::Align::Center);
        empty_buttons.append(&empty_add);
        empty_buttons.append(&empty_import);
        manage_empty.set_child(Some(&empty_buttons));

        manage_stack.add_named(&manage_page, Some("list"));
        manage_stack.add_named(&manage_empty, Some("empty"));
        manage_stack.set_visible_child_name("empty");

        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&manage_stack));
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&manage_header);
        toolbar.set_content(Some(&toasts));
        manage.set_content(Some(&toolbar));

        let import_action = gio::SimpleAction::new("import", None);
        let export_action = gio::SimpleAction::new("export", None);
        manage.add_action(&import_action);
        manage.add_action(&export_action);

        let ui = Rc::new(Self {
            app: app.clone(),
            controller,
            manage,
            manage_stack,
            manage_page,
            manage_group: RefCell::new(None),
            toasts,
            export_action: export_action.clone(),
            last_services: RefCell::new(Vec::new()),
            add_open: Cell::new(false),
        });

        let weak = Rc::downgrade(&ui);
        import_action.connect_activate(move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.import_services();
            }
        });
        let weak = Rc::downgrade(&ui);
        export_action.connect_activate(move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.export_services();
            }
        });
        let weak = Rc::downgrade(&ui);
        add_button.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.open_add_dialog();
            }
        });
        let weak = Rc::downgrade(&ui);
        empty_add.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.open_add_dialog();
            }
        });

        ui
    }

    fn install(self: &Rc<Self>) {
        self.refresh();
        let ui = self.clone();
        glib::timeout_add_local(Duration::from_millis(250), move || ui.tick());
    }

    fn tick(self: &Rc<Self>) -> glib::ControlFlow {
        let (manage, add, links, quit) = {
            let mut state = self.controller.state.lock().unwrap();
            (
                std::mem::take(&mut state.manage_requested),
                std::mem::take(&mut state.add_requested),
                std::mem::take(&mut state.open_requested),
                state.quit,
            )
        };
        if quit || crate::take_quit_signal() {
            self.app.quit();
            return glib::ControlFlow::Break;
        }
        for link in &links {
            open_link(link);
        }
        if manage {
            self.open_manage();
        }
        if add {
            self.open_manage();
            self.open_add_dialog();
        }
        self.refresh();
        glib::ControlFlow::Continue
    }

    fn is_empty(&self) -> bool {
        self.controller.state.lock().unwrap().services.is_empty()
    }

    fn open_manage(&self) {
        self.manage.present();
    }

    fn refresh(self: &Rc<Self>) {
        let services = self.controller.state.lock().unwrap().services.clone();
        if *self.last_services.borrow() != services {
            self.rebuild_manage(&services);
            *self.last_services.borrow_mut() = services;
        }
    }

    fn rebuild_manage(self: &Rc<Self>, services: &[ServiceConfig]) {
        if let Some(group) = self.manage_group.borrow_mut().take() {
            self.manage_page.remove(&group);
        }

        let group = adw::PreferencesGroup::builder()
            .title("Services")
            .description("These services appear in the tray menu. Use Edit to rename a service or to add web links to its menu. Removing an entry does not touch the systemd unit.")
            .build();

        for service in services {
            let links = match service.links.len() {
                0 => String::new(),
                1 => "  ·  1 link".to_owned(),
                count => format!("  ·  {count} links"),
            };
            let row = adw::ActionRow::builder()
                .title(service.display_name())
                .subtitle(format!(
                    "{}  ·  {} service{links}",
                    service.unit,
                    service.scope.label()
                ))
                .use_markup(false)
                .build();
            row.set_title_lines(1);
            row.set_subtitle_lines(1);
            row.add_prefix(&gtk::Image::from_icon_name("utilities-terminal-symbolic"));

            let edit = icon_button("document-edit-symbolic", "Edit");
            let remove = icon_button("user-trash-symbolic", "Remove");
            let suffix = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            suffix.set_valign(gtk::Align::Center);
            suffix.append(&edit);
            suffix.append(&remove);
            row.add_suffix(&suffix);

            let weak = Rc::downgrade(self);
            let target = service.clone();
            edit.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.open_edit_dialog(&target);
                }
            });
            let weak = Rc::downgrade(self);
            let target = service.clone();
            remove.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.open_remove_dialog(&target);
                }
            });

            group.add(&row);
        }

        self.manage_page.add(&group);
        *self.manage_group.borrow_mut() = Some(group);
        self.manage_stack
            .set_visible_child_name(if services.is_empty() { "empty" } else { "list" });
        self.export_action.set_enabled(!services.is_empty());
    }

    fn import_services(self: &Rc<Self>) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("Services Tray exports (JSON)"));
        filter.add_mime_type("application/json");
        filter.add_suffix("json");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let dialog = gtk::FileDialog::builder()
            .title("Import Services")
            .accept_label("Import")
            .modal(true)
            .filters(&filters)
            .default_filter(&filter)
            .build();
        let weak = Rc::downgrade(self);
        dialog.open(Some(&self.manage), gio::Cancellable::NONE, move |result| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            if let Some(path) = ui.chosen_path(result, "Could not import services") {
                ui.import_from(&path);
            }
        });
    }

    fn import_from(self: &Rc<Self>, path: &Path) {
        let failed = "Could not import services";
        let imported = match read_import(path) {
            Ok(imported) => imported,
            Err(error) => {
                self.show_error(failed, &format!("{error:#}"));
                return;
            }
        };
        if imported.is_empty() {
            self.toast("The file contains no services");
            return;
        }
        let conflicts = match self.controller.import_conflicts(&imported) {
            Ok(conflicts) => conflicts,
            Err(error) => {
                self.show_error(failed, &format!("{error:#}"));
                return;
            }
        };
        if conflicts.is_empty() {
            self.finish_import(imported, false);
            return;
        }

        let names: Vec<String> = conflicts.iter().map(ServiceConfig::display_name).collect();
        let dialog = gtk::AlertDialog::builder()
            .modal(true)
            .message(format!("Replace {}?", count_services(conflicts.len())))
            .detail(format!(
                "Already in the list with different settings: {}. Replace them with the imported versions, or keep the current ones and import only the new services.",
                names.join(", ")
            ))
            .buttons(["Cancel", "Keep Current", "Replace"])
            .cancel_button(0)
            .default_button(1)
            .build();
        let weak = Rc::downgrade(self);
        dialog.choose(Some(&self.manage), gio::Cancellable::NONE, move |result| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            match result {
                Ok(1) => ui.finish_import(imported, false),
                Ok(2) => ui.finish_import(imported, true),
                _ => {}
            }
        });
    }

    fn finish_import(self: &Rc<Self>, imported: Vec<ServiceConfig>, replace: bool) {
        match self.controller.import_services(imported, replace) {
            Ok(summary) => {
                self.refresh();
                self.toast(&summary.message());
            }
            Err(error) => self.show_error("Could not import services", &format!("{error:#}")),
        }
    }

    fn export_services(self: &Rc<Self>) {
        let date = glib::DateTime::now_local()
            .ok()
            .and_then(|now| now.format("%F").ok());
        let name = match date {
            Some(date) => format!("services-tray-{date}.json"),
            None => "services-tray.json".to_owned(),
        };
        let dialog = gtk::FileDialog::builder()
            .title("Export Services")
            .accept_label("Export")
            .modal(true)
            .initial_name(name)
            .build();
        let weak = Rc::downgrade(self);
        dialog.save(Some(&self.manage), gio::Cancellable::NONE, move |result| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let failed = "Could not export services";
            let Some(path) = ui.chosen_path(result, failed) else {
                return;
            };
            match ui.controller.export_services(&path) {
                Ok(count) => ui.toast(&format!("Exported {}", count_services(count))),
                Err(error) => ui.show_error(failed, &format!("{error:#}")),
            }
        });
    }

    /// The local file picked in a file dialog. Dismissing the dialog is not
    /// an error; anything else is shown to the user.
    fn chosen_path(&self, result: Result<gio::File, glib::Error>, failed: &str) -> Option<PathBuf> {
        match result {
            Ok(file) => {
                let path = file.path();
                if path.is_none() {
                    self.show_error(failed, "Choose a file on this computer.");
                }
                path
            }
            Err(error)
                if error.matches(gtk::DialogError::Dismissed)
                    || error.matches(gtk::DialogError::Cancelled) =>
            {
                None
            }
            Err(error) => {
                self.show_error(failed, error.message());
                None
            }
        }
    }

    fn show_error(&self, message: &str, detail: &str) {
        gtk::AlertDialog::builder()
            .modal(true)
            .message(message)
            .detail(detail)
            .build()
            .show(Some(&self.manage));
    }

    fn toast(&self, title: &str) {
        self.toasts.add_toast(adw::Toast::new(title));
    }

    fn open_remove_dialog(self: &Rc<Self>, service: &ServiceConfig) {
        let dialog = gtk::AlertDialog::builder()
            .modal(true)
            .message(format!(
                "Remove “{}” from Services Tray?",
                service.display_name()
            ))
            .detail("The systemd service itself will not be changed.")
            .buttons(["Cancel", "Remove"])
            .cancel_button(0)
            .default_button(0)
            .build();

        let controller = self.controller.clone();
        let key = service.key();
        let weak = Rc::downgrade(self);
        dialog.choose(
            Some(&self.manage),
            None::<&gtk::gio::Cancellable>,
            move |result| {
                if result.ok() != Some(1) {
                    return;
                }
                if let Err(error) = controller.remove_service(&key) {
                    crate::notify_error("Could not remove the service", &format!("{error:#}"));
                }
                if let Some(ui) = weak.upgrade() {
                    ui.refresh();
                }
            },
        );
    }

    fn open_edit_dialog(self: &Rc<Self>, service: &ServiceConfig) {
        let dialog = adw::Window::builder()
            .title("Edit Service")
            .transient_for(&self.manage)
            .modal(true)
            .default_width(560)
            .build();

        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&adw::WindowTitle::new("Edit Service", &service.unit)));
        let cancel = gtk::Button::with_label("Cancel");
        let save = gtk::Button::with_label("Save");
        save.add_css_class("suggested-action");
        header.pack_start(&cancel);
        header.pack_end(&save);
        dialog.set_default_widget(Some(&save));

        let content = gtk::Box::new(gtk::Orientation::Vertical, 18);
        content.set_margin_top(18);
        content.set_margin_bottom(18);
        content.set_margin_start(18);
        content.set_margin_end(18);

        let name_group = adw::PreferencesGroup::builder()
            .description("The name shown in the tray menu. Leave empty to use the unit name.")
            .build();
        let name = adw::EntryRow::builder()
            .title("Display name")
            .activates_default(true)
            .build();
        name.set_text(&service.display_name());
        name_group.add(&name);

        let add_link = gtk::Button::builder()
            .child(
                &adw::ButtonContent::builder()
                    .icon_name("list-add-symbolic")
                    .label("Add Link")
                    .build(),
            )
            .build();
        add_link.add_css_class("flat");
        let links_group = adw::PreferencesGroup::builder()
            .title("Links")
            .description("Shown in the service’s tray menu below Start, Restart and Stop; clicking one opens it in your web browser.")
            .header_suffix(&add_link)
            .build();
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .build();
        list.add_css_class("boxed-list");
        let placeholder = gtk::Label::new(Some("No links yet"));
        placeholder.add_css_class("dim-label");
        placeholder.set_margin_top(12);
        placeholder.set_margin_bottom(12);
        list.set_placeholder(Some(&placeholder));
        links_group.add(&list);

        let error = gtk::Label::new(None);
        error.set_xalign(0.0);
        error.set_wrap(true);
        error.add_css_class("error-label");
        error.set_visible(false);

        content.append(&name_group);
        content.append(&links_group);
        content.append(&error);

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&content));
        dialog.set_content(Some(&toolbar));

        let rows: Rc<RefCell<Vec<LinkRow>>> = Rc::default();
        let append_link = {
            let list = list.clone();
            let rows = rows.clone();
            move |title: &str, url: &str| -> gtk::Entry {
                let link = LinkRow::new(title, url);
                list.append(&link.row);
                let (list, row) = (list.clone(), link.row.clone());
                let weak_rows = Rc::downgrade(&rows);
                link.remove.connect_clicked(move |_| {
                    list.remove(&row);
                    if let Some(rows) = weak_rows.upgrade() {
                        rows.borrow_mut().retain(|link| link.row != row);
                    }
                });
                let focus = link.title.clone();
                rows.borrow_mut().push(link);
                focus
            }
        };
        for link in &service.links {
            append_link(&link.title, &link.url);
        }
        add_link.connect_clicked(move |_| {
            append_link("", "").grab_focus();
        });

        let key = service.key();
        let controller = self.controller.clone();
        let weak = Rc::downgrade(self);
        let dialog_clone = dialog.clone();
        let name_entry = name.clone();
        save.connect_clicked(move |_| {
            let mut links = Vec::new();
            for (index, row) in rows.borrow().iter().enumerate() {
                let (title, url) = (row.title.text(), row.url.text());
                if title.trim().is_empty() && url.trim().is_empty() {
                    continue;
                }
                match ServiceLink::new(&title, &url) {
                    Ok(link) => links.push(link),
                    Err(message) => {
                        let which = match title.trim() {
                            "" => format!("Link {}", index + 1),
                            title => format!("“{title}”"),
                        };
                        error.set_text(&format!("{which}: {message:#}"));
                        error.set_visible(true);
                        row.url.add_css_class("error");
                        row.url.grab_focus();
                        return;
                    }
                }
            }
            match controller.edit_service(&key, &name_entry.text(), links) {
                Ok(()) => {
                    if let Some(ui) = weak.upgrade() {
                        ui.refresh();
                    }
                    dialog_clone.close();
                }
                Err(message) => {
                    error.set_text(&format!("{message:#}"));
                    error.set_visible(true);
                }
            }
        });
        let dialog_clone = dialog.clone();
        cancel.connect_clicked(move |_| dialog_clone.close());

        dialog.present();
        name.grab_focus();
    }

    fn open_add_dialog(self: &Rc<Self>) {
        if self.add_open.replace(true) {
            self.open_manage();
            return;
        }
        self.open_manage();

        let dialog = adw::Window::builder()
            .title("Add Service")
            .transient_for(&self.manage)
            .modal(true)
            .default_width(500)
            .default_height(660)
            .build();

        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&adw::WindowTitle::new("Add Service", "")));
        let cancel = gtk::Button::with_label("Cancel");
        let add = gtk::Button::with_label("Add");
        add.add_css_class("suggested-action");
        header.pack_start(&cancel);
        header.pack_end(&add);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.set_margin_top(16);
        content.set_margin_bottom(16);
        content.set_margin_start(16);
        content.set_margin_end(16);

        let intro = gtk::Label::new(Some(
            "Choose a user or system service. You can also type a unit name that is not listed.",
        ));
        intro.set_xalign(0.0);
        intro.set_wrap(true);
        intro.add_css_class("dim-label");

        let scope_group = adw::PreferencesGroup::new();
        let scope_row = adw::ActionRow::builder()
            .title("Scope")
            .subtitle("User services need no administrator authentication")
            .build();
        let scope = gtk::DropDown::from_strings(&["User service", "System service"]);
        scope.set_valign(gtk::Align::Center);
        scope_row.add_suffix(&scope);
        scope_row.set_activatable_widget(Some(&scope));
        scope_group.add(&scope_row);

        let search = gtk::SearchEntry::new();
        search.set_placeholder_text(Some("Search installed services…"));

        // A single click only selects, so a display name can still be typed;
        // double-click or Enter adds the service right away.
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .activate_on_single_click(false)
            .build();
        list.add_css_class("boxed-list");
        let scroller = gtk::ScrolledWindow::builder()
            .height_request(300)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        scroller.set_child(Some(&list));

        let name_group = adw::PreferencesGroup::new();
        let name = adw::EntryRow::builder()
            .title("Display name (optional)")
            .build();
        name_group.add(&name);

        let error = gtk::Label::new(None);
        error.set_xalign(0.0);
        error.set_wrap(true);
        error.add_css_class("error-label");
        error.set_visible(false);

        content.append(&intro);
        content.append(&scope_group);
        content.append(&search);
        content.append(&scroller);
        content.append(&name_group);
        content.append(&error);

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&content));
        dialog.set_content(Some(&toolbar));

        let entries: Rc<RefCell<Vec<UnitEntry>>> = Rc::new(RefCell::new(Vec::new()));
        let reload: Rc<dyn Fn()> = {
            let list = list.clone();
            let entries = entries.clone();
            let scope = scope.clone();
            let search = search.clone();
            let error = error.clone();
            Rc::new(move || {
                while let Some(child) = list.first_child() {
                    list.remove(&child);
                }
                entries.borrow_mut().clear();
                list.unselect_all();
                error.set_visible(false);

                let scope_value = if scope.selected() == 1 {
                    Scope::System
                } else {
                    Scope::User
                };
                match crate::systemd::list_units(scope_value) {
                    Ok(units) => {
                        for unit in units {
                            let row = gtk::ListBoxRow::new();
                            let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                            line.set_margin_top(7);
                            line.set_margin_bottom(7);
                            line.set_margin_start(10);
                            line.set_margin_end(10);
                            let unit_label = gtk::Label::new(Some(&unit.unit));
                            unit_label.set_xalign(0.0);
                            unit_label.set_hexpand(true);
                            unit_label.set_ellipsize(pango::EllipsizeMode::End);
                            let state_label = gtk::Label::new(Some(&unit.state));
                            state_label.add_css_class("caption");
                            state_label.add_css_class("dim-label");
                            line.append(&unit_label);
                            line.append(&state_label);
                            row.set_child(Some(&line));
                            list.append(&row);
                            entries.borrow_mut().push(UnitEntry {
                                row,
                                unit: unit.unit,
                            });
                        }
                    }
                    Err(message) => {
                        error.set_text(&format!("Could not list services: {message}"));
                        error.set_visible(true);
                    }
                }
                apply_filter(&entries.borrow(), &search.text());
            })
        };

        let action = reload.clone();
        scope.connect_selected_notify(move |_| action());

        let filter_entries = entries.clone();
        let filter_list = list.clone();
        search.connect_search_changed(move |search| {
            apply_filter(&filter_entries.borrow(), &search.text());
            if let Some(row) = filter_list.selected_row() {
                if !row.is_visible() {
                    filter_list.unselect_all();
                }
            }
        });

        let add_action: Rc<dyn Fn()> = {
            let dialog = dialog.clone();
            let list = list.clone();
            let entries = entries.clone();
            let scope = scope.clone();
            let search = search.clone();
            let name = name.clone();
            let error = error.clone();
            let controller = self.controller.clone();
            let weak = Rc::downgrade(self);
            Rc::new(move || {
                let selected = list.selected_row().and_then(|row| {
                    entries
                        .borrow()
                        .iter()
                        .find(|entry| entry.row == row)
                        .map(|entry| entry.unit.clone())
                });
                let typed = search.text().to_string();
                let typed = typed.trim();
                let unit = selected.or_else(|| (!typed.is_empty()).then(|| typed.to_owned()));
                let Some(unit) = unit else {
                    error.set_text("Choose a service from the list or type its unit name.");
                    error.set_visible(true);
                    return;
                };

                let scope_value = if scope.selected() == 1 {
                    Scope::System
                } else {
                    Scope::User
                };
                let display_name = name.text().to_string();
                let service = ServiceConfig::new(&unit, scope_value, Some(display_name.as_str()));
                match controller.add_service(service) {
                    Ok(()) => {
                        if let Some(ui) = weak.upgrade() {
                            ui.add_open.set(false);
                            ui.refresh();
                        }
                        dialog.destroy();
                    }
                    Err(message) => {
                        error.set_text(&format!("{message:#}"));
                        error.set_visible(true);
                    }
                }
            })
        };

        let action = add_action.clone();
        add.connect_clicked(move |_| action());
        let action = add_action.clone();
        search.connect_activate(move |_| action());
        let action = add_action.clone();
        name.connect_entry_activated(move |_| action());
        let action = add_action.clone();
        list.connect_row_activated(move |_, _| action());

        let dialog_clone = dialog.clone();
        let weak = Rc::downgrade(self);
        cancel.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.add_open.set(false);
            }
            dialog_clone.destroy();
        });

        let weak = Rc::downgrade(self);
        dialog.connect_close_request(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.add_open.set(false);
            }
            glib::Propagation::Proceed
        });

        let weak = Rc::downgrade(self);
        dialog.connect_destroy(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.add_open.set(false);
            }
        });

        reload();
        dialog.present();
        search.grab_focus();
    }
}

struct UnitEntry {
    row: gtk::ListBoxRow,
    unit: String,
}

struct LinkRow {
    row: gtk::ListBoxRow,
    title: gtk::Entry,
    url: gtk::Entry,
    remove: gtk::Button,
}

impl LinkRow {
    fn new(title: &str, url: &str) -> Self {
        let title_entry = gtk::Entry::builder()
            .text(title)
            .placeholder_text("Title")
            .width_chars(12)
            .activates_default(true)
            .build();
        title_entry.update_property(&[gtk::accessible::Property::Label("Link title")]);
        let url_entry = gtk::Entry::builder()
            .text(url)
            .placeholder_text("http://127.0.0.1:8080")
            .input_purpose(gtk::InputPurpose::Url)
            .hexpand(true)
            .activates_default(true)
            .build();
        url_entry.update_property(&[gtk::accessible::Property::Label("Link address")]);
        url_entry.connect_changed(|entry| entry.remove_css_class("error"));
        let remove = icon_button("user-trash-symbolic", "Remove link");
        remove.set_valign(gtk::Align::Center);

        let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        line.set_margin_top(8);
        line.set_margin_bottom(8);
        line.set_margin_start(10);
        line.set_margin_end(8);
        line.append(&title_entry);
        line.append(&url_entry);
        line.append(&remove);
        let row = gtk::ListBoxRow::builder()
            .activatable(false)
            .child(&line)
            .build();
        Self {
            row,
            title: title_entry,
            url: url_entry,
            remove,
        }
    }
}

fn open_link(link: &ServiceLink) {
    let failed = format!("Could not open {}", link.label());
    // The file can be edited by hand, so the address is checked again here.
    let url = match normalize_url(&link.url) {
        Ok(url) => url,
        Err(error) => {
            crate::notify_error(&failed, &format!("{error:#}"));
            return;
        }
    };
    gtk::UriLauncher::new(&url).launch(
        None::<&gtk::Window>,
        gio::Cancellable::NONE,
        move |result| {
            if let Err(error) = result {
                crate::notify_error(&failed, error.message());
            }
        },
    );
}

fn apply_filter(entries: &[UnitEntry], query: &str) {
    let query = query.trim().to_lowercase();
    for entry in entries {
        entry
            .row
            .set_visible(query.is_empty() || entry.unit.to_lowercase().contains(&query));
    }
}

fn icon_button(icon_name: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(icon_name);
    button.set_tooltip_text(Some(tooltip));
    button.update_property(&[gtk::accessible::Property::Label(tooltip)]);
    button.add_css_class("flat");
    button.add_css_class("circular");
    button
}
