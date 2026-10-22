use crate::model::{Action, Scope, ServiceConfig, Status};
use std::collections::HashMap;
use std::process::{Command, Output, Stdio};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnitChoice {
    pub unit: String,
    pub state: String,
}

pub fn statuses(scope: Scope, units: &[String]) -> HashMap<String, Status> {
    if units.is_empty() {
        return HashMap::new();
    }

    let mut command = Command::new("systemctl");
    command.args(scope.systemctl_args()).arg("show");
    for property in ["Names", "ActiveState", "LoadState"] {
        command.arg(format!("--property={property}"));
    }
    let output = command
        .args(["--no-pager", "--"])
        .args(units)
        .stdin(Stdio::null())
        .output();
    let text = output
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default();
    parse_show(&text, units)
}

/// Matches blocks by `Names=` because a unit requested through an alias
/// (`dbus.service`) is reported with its primary `Id=` (`dbus-broker.service`).
fn parse_show(text: &str, units: &[String]) -> HashMap<String, Status> {
    let mut result: HashMap<String, Status> = units
        .iter()
        .map(|unit| (unit.clone(), Status::Unknown))
        .collect();
    for block in text.split("\n\n") {
        let mut names = Vec::new();
        let mut active = "";
        let mut load = "";
        for line in block.lines() {
            if let Some(value) = line.strip_prefix("Names=") {
                names = value.split_whitespace().collect();
            } else if let Some(value) = line.strip_prefix("ActiveState=") {
                active = value.trim();
            } else if let Some(value) = line.strip_prefix("LoadState=") {
                load = value.trim();
            }
        }
        for name in names {
            if let Some(status) = result.get_mut(name) {
                *status = status_from(active, load);
            }
        }
    }
    result
}

pub fn run_action(service: &ServiceConfig, action: Action) -> Result<(), String> {
    let output = systemctl(service.scope, action.verb(), &service.unit)?;
    if output.status.success() {
        return Ok(());
    }

    let message = stderr(&output);
    if service.scope == Scope::System && needs_polkit(&message) {
        return run_with_pkexec(action, &service.unit);
    }
    Err(message)
}

pub fn list_units(scope: Scope) -> Result<Vec<UnitChoice>, String> {
    let output = Command::new("systemctl")
        .args(scope.systemctl_args())
        .args([
            "list-unit-files",
            "--type=service",
            "--no-legend",
            "--no-pager",
            "--plain",
        ])
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("Could not run systemctl: {error}"))?;

    if !output.status.success() {
        return Err(stderr(&output));
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let mut units: Vec<UnitChoice> = text
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let unit = fields.next()?.trim_start_matches(['●', '*', ' ']);
            let state = fields.next().unwrap_or("");
            if !unit.ends_with(".service") || unit.ends_with("@.service") {
                return None;
            }
            Some(UnitChoice {
                unit: unit.to_owned(),
                state: state.to_owned(),
            })
        })
        .collect();
    units.sort_by(|a, b| a.unit.cmp(&b.unit));
    units.dedup_by(|a, b| a.unit == b.unit);
    Ok(units)
}

fn systemctl(scope: Scope, verb: &str, unit: &str) -> Result<Output, String> {
    Command::new("systemctl")
        .args(scope.systemctl_args())
        .args([verb, "--", unit])
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("Could not run systemctl: {error}"))
}

fn run_with_pkexec(action: Action, unit: &str) -> Result<(), String> {
    let output = Command::new("pkexec")
        .args(["systemctl", action.verb(), "--", unit])
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("Could not run pkexec: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(stderr(&output))
    }
}

fn needs_polkit(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("interactive authentication required")
        || message.contains("access denied")
        || message.contains("not authorized")
}

fn stderr(output: &Output) -> String {
    let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if message.is_empty() {
        format!("systemctl exited with {}", output.status)
    } else {
        message
    }
}

fn status_from(active: &str, load: &str) -> Status {
    if load == "not-found" {
        return Status::NotFound;
    }
    match active {
        "active" => Status::Active,
        "activating" => Status::Activating,
        "deactivating" => Status::Deactivating,
        "failed" => Status::Failed,
        "inactive" => Status::Inactive,
        _ => Status::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_systemd_states() {
        assert_eq!(status_from("active", "loaded"), Status::Active);
        assert_eq!(status_from("activating", "loaded"), Status::Activating);
        assert_eq!(status_from("deactivating", "loaded"), Status::Deactivating);
        assert_eq!(status_from("failed", "loaded"), Status::Failed);
        assert_eq!(status_from("inactive", "loaded"), Status::Inactive);
        assert_eq!(status_from("inactive", "not-found"), Status::NotFound);
        assert_eq!(status_from("whatever", "loaded"), Status::Unknown);
    }

    #[test]
    fn statuses_match_units_requested_through_an_alias() {
        let output =
            "Names=dbus-broker.service dbus.service\nLoadState=loaded\nActiveState=active\n\n\
                      Names=gone.service\nLoadState=not-found\nActiveState=inactive\n";
        let units = ["dbus.service", "gone.service", "silent.service"].map(String::from);
        let statuses = parse_show(output, &units);
        assert_eq!(statuses.len(), 3);
        assert_eq!(statuses["dbus.service"], Status::Active);
        assert_eq!(statuses["gone.service"], Status::NotFound);
        assert_eq!(statuses["silent.service"], Status::Unknown);
    }

    #[test]
    fn detects_polkit_errors() {
        assert!(needs_polkit(
            "Failed to restart sshd.service: Interactive authentication required."
        ));
        assert!(needs_polkit("Access denied"));
        assert!(!needs_polkit("Unit sshd.service not found."));
    }
}
