use anyhow::{anyhow, bail, Context, Result};
use gtk4::glib;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const APP_NAME: &str = "Services Tray";
pub const APP_ID: &str = "io.github.servicestray.ServicesTray";
pub const CONFIG_ENV: &str = "SERVICES_TRAY_CONFIG";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    #[default]
    User,
    System,
}

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Self::User => "User",
            Self::System => "System",
        }
    }

    pub fn systemctl_args(self) -> &'static [&'static str] {
        match self {
            Self::User => &["--user"],
            Self::System => &[],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceConfig {
    pub unit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub scope: Scope,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<ServiceLink>,
}

impl ServiceConfig {
    pub fn new(unit: &str, scope: Scope, name: Option<&str>) -> Self {
        let name = name.map(str::trim).filter(|name| !name.is_empty());
        Self {
            unit: normalize_unit(unit),
            name: name.map(str::to_owned),
            scope,
            links: Vec::new(),
        }
    }

    pub fn display_name(&self) -> String {
        match &self.name {
            Some(name) if !name.trim().is_empty() => name.trim().to_owned(),
            _ => self.default_name().to_owned(),
        }
    }

    fn default_name(&self) -> &str {
        self.unit.strip_suffix(".service").unwrap_or(&self.unit)
    }

    /// An empty name, or the unit's own name, removes the custom name.
    pub fn set_name(&mut self, name: &str) {
        let name = name.trim();
        self.name = (!name.is_empty() && name != self.default_name()).then(|| name.to_owned());
    }

    pub fn key(&self) -> ServiceKey {
        ServiceKey {
            scope: self.scope,
            unit: self.unit.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ServiceKey {
    pub scope: Scope,
    pub unit: String,
}

/// A web page of a service, opened from the service's tray menu.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceLink {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    pub url: String,
}

impl ServiceLink {
    pub fn new(title: &str, url: &str) -> Result<Self> {
        Ok(Self {
            title: title.trim().to_owned(),
            url: normalize_url(url)?,
        })
    }

    pub fn label(&self) -> &str {
        match self.title.trim() {
            "" => self.url.trim(),
            title => title,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Status {
    Active,
    Activating,
    Deactivating,
    Failed,
    Inactive,
    NotFound,
    #[default]
    Unknown,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Self::Active => "Running",
            Self::Activating => "Starting…",
            Self::Deactivating => "Stopping…",
            Self::Failed => "Failed",
            Self::Inactive => "Stopped",
            Self::NotFound => "Not found",
            Self::Unknown => "Unknown",
        }
    }

    pub fn is_active(self) -> bool {
        matches!(self, Self::Active | Self::Activating | Self::Deactivating)
    }

    pub fn can_start(self) -> bool {
        matches!(self, Self::Inactive | Self::Failed)
    }

    pub fn can_stop(self) -> bool {
        self.is_active()
    }

    pub fn can_restart(self) -> bool {
        matches!(self, Self::Active)
    }
}

#[derive(Default)]
pub struct AppState {
    pub services: Vec<ServiceConfig>,
    pub statuses: HashMap<ServiceKey, Status>,
    pub busy: HashSet<ServiceKey>,
    pub manage_requested: bool,
    pub add_requested: bool,
    pub open_requested: Vec<ServiceLink>,
    pub quit: bool,
}

pub type SharedState = Arc<Mutex<AppState>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Start,
    Stop,
    Restart,
}

impl Action {
    pub fn verb(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
        }
    }
}

#[derive(Serialize, Deserialize, Default)]
struct ConfigFile {
    #[serde(default)]
    services: Vec<ServiceConfig>,
}

pub fn config_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(CONFIG_ENV) {
        return Ok(path.into());
    }
    Ok(dirs::config_dir()
        .context("cannot determine the configuration directory")?
        .join("services-tray")
        .join("services.json"))
}

pub fn load_services() -> Result<Vec<ServiceConfig>> {
    let path = config_path()?;
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("cannot read {}", path.display())),
    };
    if contents.trim().is_empty() {
        return Ok(Vec::new());
    }
    let file: ConfigFile = serde_json::from_str(&contents)
        .with_context(|| format!("cannot parse {}", path.display()))?;
    Ok(clean(file.services))
}

pub fn save_services(services: &[ServiceConfig]) -> Result<()> {
    write_services(&config_path()?, services)
}

/// Writes `services` in the configuration file format; exports use it too,
/// so an export can also be dropped in as `services.json`.
pub fn write_services(path: &Path, services: &[ServiceConfig]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let contents = serde_json::to_string_pretty(&ConfigFile {
        services: services.to_vec(),
    })?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, format!("{contents}\n"))
        .with_context(|| format!("cannot write {}", temporary.display()))?;
    std::fs::rename(&temporary, path)
        .with_context(|| format!("cannot replace {}", path.display()))?;
    Ok(())
}

/// Real exports are a few KiB; this keeps a wrongly chosen file from being
/// read into memory.
const MAX_IMPORT_BYTES: u64 = 1024 * 1024;

/// Reads an exported file and checks every entry the way typed input is
/// checked, so an import cannot add invalid units or non-web links.
pub fn read_import(path: &Path) -> Result<Vec<ServiceConfig>> {
    let size = std::fs::metadata(path)
        .with_context(|| format!("cannot read {}", path.display()))?
        .len();
    if size > MAX_IMPORT_BYTES {
        bail!(
            "{} is too large to be a Services Tray export.",
            path.display()
        );
    }
    let contents =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let file: ConfigFile = serde_json::from_str(&contents)
        .with_context(|| format!("{} is not a Services Tray export", path.display()))?;
    clean(file.services)
        .into_iter()
        .map(|mut service| {
            validate_unit(&service.unit)
                .with_context(|| format!("invalid service “{}”", service.unit))?;
            let name = service.name.take().unwrap_or_default();
            service.set_name(&name);
            service.links = service
                .links
                .iter()
                .map(|link| ServiceLink::new(&link.title, &link.url))
                .collect::<Result<Vec<_>>>()
                .with_context(|| format!("invalid link in “{}”", service.unit))?;
            Ok(service)
        })
        .collect()
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ImportSummary {
    pub added: usize,
    pub replaced: usize,
}

impl ImportSummary {
    pub fn message(&self) -> String {
        match (self.added, self.replaced) {
            (0, 0) => "All imported services were already in the list".to_owned(),
            (added, 0) => format!("Imported {}", count_services(added)),
            (0, replaced) => format!("Replaced {}", count_services(replaced)),
            (added, replaced) => format!(
                "Imported {} and replaced {}",
                count_services(added),
                count_services(replaced)
            ),
        }
    }
}

pub fn count_services(count: usize) -> String {
    match count {
        1 => "1 service".to_owned(),
        count => format!("{count} services"),
    }
}

/// Adds the imported services that are not in `services` yet. Entries that
/// are already there are replaced only when `replace` is set.
fn merge(
    services: &mut Vec<ServiceConfig>,
    imported: Vec<ServiceConfig>,
    replace: bool,
) -> ImportSummary {
    let mut summary = ImportSummary::default();
    for service in imported {
        match services
            .iter_mut()
            .find(|existing| existing.key() == service.key())
        {
            None => {
                services.push(service);
                summary.added += 1;
            }
            Some(existing) if replace && *existing != service => {
                *existing = service;
                summary.replaced += 1;
            }
            Some(_) => {}
        }
    }
    summary
}

pub fn normalize_unit(unit: &str) -> String {
    let unit = unit.trim();
    if unit.ends_with(".service") {
        unit.to_owned()
    } else {
        format!("{unit}.service")
    }
}

pub fn validate_unit(unit: &str) -> Result<()> {
    let unit = unit.trim();
    if unit.strip_suffix(".service").unwrap_or(unit).is_empty() {
        bail!("Enter a service name.");
    }
    if unit.chars().any(char::is_whitespace) {
        bail!("A service name cannot contain spaces.");
    }
    if unit.contains('/') {
        bail!("A service name cannot contain a slash.");
    }
    if unit.starts_with('-') {
        bail!("A service name cannot start with a dash.");
    }
    Ok(())
}

/// Checks a link address and returns it as stored. Only web addresses are
/// accepted; one typed without a scheme, like `127.0.0.1:9001`, gets `http://`.
pub fn normalize_url(url: &str) -> Result<String> {
    let url = url.trim();
    if url.is_empty() {
        bail!("Enter the link address.");
    }
    // GLib's parser accepts these, but no browser address may contain them.
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        bail!("A link address cannot contain spaces.");
    }
    let has_scheme = url.split_once("://").is_some_and(|(scheme, _)| {
        scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    });
    let url = if has_scheme {
        url.to_owned()
    } else {
        format!("http://{url}")
    };
    let parsed = glib::Uri::parse(&url, glib::UriFlags::NONE)
        .map_err(|_| anyhow!("“{url}” is not a valid address."))?;
    if !matches!(parsed.scheme().as_str(), "http" | "https") {
        bail!("Only http:// and https:// addresses are supported.");
    }
    if parsed.host().is_none_or(|host| host.is_empty()) {
        bail!("“{url}” has no host name.");
    }
    Ok(url)
}

fn clean(services: Vec<ServiceConfig>) -> Vec<ServiceConfig> {
    let mut seen = HashSet::new();
    services
        .into_iter()
        .filter(|service| !service.unit.trim().is_empty())
        .map(|mut service| {
            service.unit = normalize_unit(&service.unit);
            service
        })
        .filter(|service| seen.insert(service.key()))
        .collect()
}

pub struct Controller {
    pub state: SharedState,
    tray: Mutex<Option<crate::tray::Handle>>,
}

impl Controller {
    pub fn new(state: SharedState) -> Self {
        Self {
            state,
            tray: Mutex::new(None),
        }
    }

    pub fn set_tray(&self, handle: crate::tray::Handle) {
        *self.tray.lock().unwrap() = Some(handle);
    }

    pub fn refresh_tray(&self) {
        if let Some(handle) = self.tray.lock().unwrap().as_ref() {
            handle.update(|_| {});
        }
    }

    pub fn request_manage(&self) {
        self.state.lock().unwrap().manage_requested = true;
    }

    pub fn request_add(&self) {
        let mut state = self.state.lock().unwrap();
        state.manage_requested = true;
        state.add_requested = true;
    }

    pub fn request_open(&self, link: ServiceLink) {
        self.state.lock().unwrap().open_requested.push(link);
    }

    pub fn has_tray(&self) -> bool {
        self.tray.lock().unwrap().is_some()
    }

    pub fn quit(&self) {
        self.state.lock().unwrap().quit = true;
    }

    pub fn should_quit(&self) -> bool {
        self.state.lock().unwrap().quit
    }

    pub fn poll(&self) -> bool {
        let services = self.state.lock().unwrap().services.clone();
        let mut statuses = HashMap::new();
        for scope in [Scope::User, Scope::System] {
            let units: Vec<String> = services
                .iter()
                .filter(|service| service.scope == scope)
                .map(|service| service.unit.clone())
                .collect();
            if units.is_empty() {
                continue;
            }
            for (unit, status) in crate::systemd::statuses(scope, &units) {
                statuses.insert(ServiceKey { scope, unit }, status);
            }
        }
        let mut state = self.state.lock().unwrap();
        if state.statuses == statuses {
            false
        } else {
            state.statuses = statuses;
            true
        }
    }

    pub fn action(self: &Arc<Self>, service: ServiceConfig, action: Action) {
        let key = service.key();
        {
            let mut state = self.state.lock().unwrap();
            if !state.busy.insert(key.clone()) {
                return;
            }
        }

        let controller = self.clone();
        std::thread::spawn(move || {
            if let Err(error) = crate::systemd::run_action(&service, action) {
                crate::notify_error(
                    &format!("Could not {} {}", action.label(), service.display_name()),
                    &error,
                );
            }
            controller.poll();
            let mut state = controller.state.lock().unwrap();
            state.busy.remove(&key);
            drop(state);
            controller.refresh_tray();
        });
    }

    // Changes are applied to the configuration file as it is on disk rather
    // than to the in-memory list, so edits made to the file while the tray
    // runs are kept and a file that cannot be parsed is never overwritten.

    pub fn add_service(&self, service: ServiceConfig) -> Result<()> {
        validate_unit(&service.unit)?;
        let mut services = load_services()?;
        if services
            .iter()
            .any(|existing| existing.key() == service.key())
        {
            bail!("{} is already in the list.", service.unit);
        }
        services.push(service);
        self.store(services)
    }

    pub fn remove_service(&self, key: &ServiceKey) -> Result<()> {
        let mut services = load_services()?;
        services.retain(|service| service.key() != *key);
        self.store(services)
    }

    pub fn edit_service(
        &self,
        key: &ServiceKey,
        name: &str,
        links: Vec<ServiceLink>,
    ) -> Result<()> {
        let mut services = load_services()?;
        let service = services
            .iter_mut()
            .find(|service| service.key() == *key)
            .with_context(|| format!("{} is no longer in the list.", key.unit))?;
        service.set_name(name);
        service.links = links;
        self.store(services)
    }

    /// Imported services that are already in the list with other settings.
    pub fn import_conflicts(&self, imported: &[ServiceConfig]) -> Result<Vec<ServiceConfig>> {
        let services = load_services()?;
        Ok(imported
            .iter()
            .filter(|service| {
                services
                    .iter()
                    .any(|existing| existing.key() == service.key() && existing != *service)
            })
            .cloned()
            .collect())
    }

    pub fn import_services(
        &self,
        imported: Vec<ServiceConfig>,
        replace: bool,
    ) -> Result<ImportSummary> {
        let mut services = load_services()?;
        let summary = merge(&mut services, imported, replace);
        if summary != ImportSummary::default() {
            self.store(services)?;
        }
        Ok(summary)
    }

    /// Exports the list shown in the app, which still works when the file on
    /// disk has become unreadable.
    pub fn export_services(&self, path: &Path) -> Result<usize> {
        let services = self.state.lock().unwrap().services.clone();
        write_services(path, &services)?;
        Ok(services.len())
    }

    fn store(&self, services: Vec<ServiceConfig>) -> Result<()> {
        save_services(&services)?;
        self.state.lock().unwrap().services = services;
        self.poll();
        self.refresh_tray();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_names_are_normalized() {
        assert_eq!(normalize_unit("sshd"), "sshd.service");
        assert_eq!(normalize_unit(" sshd.service "), "sshd.service");
        assert_eq!(normalize_unit("user@1000.service"), "user@1000.service");
    }

    #[test]
    fn display_name_uses_the_unit_by_default() {
        let service = ServiceConfig::new("sshd", Scope::User, None);
        assert_eq!(service.display_name(), "sshd");
        assert_eq!(service.unit, "sshd.service");

        let renamed = ServiceConfig::new("sshd", Scope::System, Some(" SSH "));
        assert_eq!(renamed.display_name(), "SSH");
    }

    #[test]
    fn status_actions_follow_the_lifecycle() {
        assert!(Status::Inactive.can_start());
        assert!(!Status::Inactive.can_stop());
        assert!(Status::Active.can_stop());
        assert!(Status::Active.can_restart());
        assert!(!Status::Active.can_start());
        assert!(!Status::NotFound.can_start());
        assert!(!Status::Failed.can_restart());
    }

    #[test]
    fn invalid_units_are_rejected() {
        assert!(validate_unit("").is_err());
        assert!(validate_unit(".service").is_err());
        assert!(validate_unit("my service").is_err());
        assert!(validate_unit("../unit").is_err());
        assert!(validate_unit("-x.service").is_err());
        assert!(validate_unit("sshd").is_ok());
    }

    #[test]
    fn names_equal_to_the_unit_are_not_stored() {
        let mut service = ServiceConfig::new("sshd", Scope::System, None);
        service.set_name(" sshd ");
        assert_eq!(service.name, None);
        service.set_name(" SSH ");
        assert_eq!(service.name.as_deref(), Some("SSH"));
        service.set_name("");
        assert_eq!(service.name, None);
    }

    #[test]
    fn link_addresses_are_validated() {
        let valid = [
            (" http://127.0.0.1:80 ", "http://127.0.0.1:80"),
            ("127.0.0.1:9001", "http://127.0.0.1:9001"),
            (
                "localhost:8888/gui?next=http://x",
                "http://localhost:8888/gui?next=http://x",
            ),
            ("HTTPS://[::1]:8443/", "HTTPS://[::1]:8443/"),
        ];
        for (input, expected) in valid {
            assert_eq!(normalize_url(input).unwrap(), expected, "{input}");
        }
        for input in [
            "",
            "ftp://example.com",
            "file:///etc/passwd",
            "http://",
            "http://exa mple.com",
        ] {
            assert!(normalize_url(input).is_err(), "{input}");
        }
    }

    #[test]
    fn links_without_a_title_show_the_address() {
        let link = ServiceLink::new(" ", "127.0.0.1:9001").unwrap();
        assert_eq!(link.label(), "http://127.0.0.1:9001");
        let link = ServiceLink::new(" Dashboard ", "127.0.0.1:9001").unwrap();
        assert_eq!(link.label(), "Dashboard");
    }

    #[test]
    fn links_are_optional_in_the_config_file() {
        let file: ConfigFile =
            serde_json::from_str(r#"{"services":[{"unit":"sshd.service","scope":"system"}]}"#)
                .unwrap();
        assert!(file.services[0].links.is_empty());
        assert!(!serde_json::to_string(&file).unwrap().contains("links"));

        let mut service = ServiceConfig::new("httpd", Scope::System, None);
        service.links = vec![ServiceLink::new("Home Page", "http://127.0.0.1:80").unwrap()];
        let services = vec![service];
        let json = serde_json::to_string(&ConfigFile {
            services: services.clone(),
        })
        .unwrap();
        let parsed: ConfigFile = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.services, services);
    }

    static CONFIG_LOCK: Mutex<()> = Mutex::new(());

    fn with_config(contents: &str, test: impl FnOnce(&std::path::Path)) {
        let _guard = CONFIG_LOCK.lock().unwrap();
        let dir =
            std::env::temp_dir().join(format!("services-tray-config-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("services.json");
        std::fs::write(&path, contents).unwrap();
        std::env::set_var(CONFIG_ENV, &path);
        test(&path);
        std::env::remove_var(CONFIG_ENV);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn an_empty_config_file_has_no_services() {
        with_config(" \n", |_| assert!(load_services().unwrap().is_empty()));
    }

    #[test]
    fn a_config_file_that_cannot_be_parsed_is_not_overwritten() {
        with_config("{ broken", |path| {
            let controller = Controller::new(SharedState::default());
            let service = ServiceConfig::new("sshd", Scope::System, None);
            let error = controller.add_service(service).unwrap_err();
            assert!(format!("{error:#}").contains("cannot parse"));
            assert_eq!(std::fs::read_to_string(path).unwrap(), "{ broken");
        });
    }

    #[test]
    fn changes_keep_edits_made_to_the_file() {
        with_config(r#"{"services":[{"unit":"edited.service"}]}"#, |_| {
            let controller = Controller::new(SharedState::default());
            let mut service = ServiceConfig::new("httpd", Scope::System, None);
            controller.add_service(service.clone()).unwrap();
            let units: Vec<String> = load_services()
                .unwrap()
                .into_iter()
                .map(|service| service.unit)
                .collect();
            assert_eq!(units, ["edited.service", "httpd.service"]);

            let link = ServiceLink::new("Home Page", "127.0.0.1:80").unwrap();
            controller
                .edit_service(&service.key(), "Apache", vec![link.clone()])
                .unwrap();
            service.name = Some("Apache".into());
            service.links = vec![link];
            assert_eq!(load_services().unwrap()[1], service);
            assert_eq!(controller.state.lock().unwrap().services[1], service);
        });
    }

    fn scratch_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("services-tray-{name}-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn exports_import_back_unchanged() {
        let dir = scratch_dir("export");
        let path = dir.join("backup.json");
        let mut apache = ServiceConfig::new("httpd", Scope::System, Some("Apache"));
        apache.links = vec![ServiceLink::new("Home Page", "http://127.0.0.1:80").unwrap()];
        let services = vec![
            apache,
            ServiceConfig::new("resilio-sync", Scope::User, None),
        ];
        write_services(&path, &services).unwrap();
        assert_eq!(read_import(&path).unwrap(), services);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn imports_are_checked_like_typed_input() {
        let dir = scratch_dir("import");
        let path = dir.join("import.json");
        let read = |json: &str| {
            std::fs::write(&path, json).unwrap();
            read_import(&path)
        };

        let services = read(
            r#"{"services":[{"unit":"httpd","name":" httpd ","scope":"system",
                "links":[{"title":" Home ","url":"127.0.0.1:80"}]}]}"#,
        )
        .unwrap();
        assert_eq!(services[0].unit, "httpd.service");
        assert_eq!(services[0].name, None);
        assert_eq!(
            services[0].links,
            [ServiceLink {
                title: "Home".into(),
                url: "http://127.0.0.1:80".into()
            }]
        );
        assert!(read(r#"{"services":[]}"#).unwrap().is_empty());
        assert!(read(r#"{"something":"else"}"#).unwrap().is_empty());

        for bad in [
            "not json",
            r#"{"services":[{"unit":"-x"}]}"#,
            r#"{"services":[{"unit":"my unit"}]}"#,
            r#"{"services":[{"unit":"httpd","links":[{"url":"file:///etc/passwd"}]}]}"#,
        ] {
            assert!(read(bad).is_err(), "{bad}");
        }
        std::fs::write(&path, vec![b' '; MAX_IMPORT_BYTES as usize + 1]).unwrap();
        assert!(read_import(&path).is_err());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn imports_add_new_services_and_replace_only_on_request() {
        let current = ServiceConfig::new("httpd", Scope::System, Some("Apache"));
        let mut changed = current.clone();
        changed.links = vec![ServiceLink::new("Home Page", "127.0.0.1:80").unwrap()];
        let new = ServiceConfig::new("smb", Scope::System, None);
        let imported = vec![changed.clone(), new.clone()];

        let mut services = vec![current.clone()];
        let summary = merge(&mut services, imported.clone(), false);
        assert_eq!(
            summary,
            ImportSummary {
                added: 1,
                replaced: 0
            }
        );
        assert_eq!(services, [current, new.clone()]);

        let summary = merge(&mut services, imported.clone(), true);
        assert_eq!(
            summary,
            ImportSummary {
                added: 0,
                replaced: 1
            }
        );
        assert_eq!(services, [changed, new]);

        assert_eq!(
            merge(&mut services, imported, true),
            ImportSummary::default()
        );
        assert_eq!(
            ImportSummary {
                added: 2,
                replaced: 1
            }
            .message(),
            "Imported 2 services and replaced 1 service"
        );
    }

    #[test]
    fn import_reports_conflicts_and_keeps_current_entries() {
        with_config(
            r#"{"services":[{"unit":"httpd.service","name":"Apache","scope":"system"}]}"#,
            |_| {
                let controller = Controller::new(SharedState::default());
                let renamed = ServiceConfig::new("httpd", Scope::System, Some("Web"));
                let new = ServiceConfig::new("smb", Scope::System, None);
                let imported = vec![renamed.clone(), new];
                assert_eq!(controller.import_conflicts(&imported).unwrap(), [renamed]);

                let summary = controller.import_services(imported, false).unwrap();
                assert_eq!(
                    summary,
                    ImportSummary {
                        added: 1,
                        replaced: 0
                    }
                );
                let names: Vec<String> = load_services()
                    .unwrap()
                    .iter()
                    .map(ServiceConfig::display_name)
                    .collect();
                assert_eq!(names, ["Apache", "smb"]);
            },
        );
    }
}
