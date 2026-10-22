use anyhow::{Context, Result};
use gtk4::glib;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

const FILE_NAME: &str = "services-tray.desktop";
const DIR_ENV: &str = "SERVICES_TRAY_AUTOSTART_DIR";
const COMMAND: &str = "services-tray";
const GROUP: &str = "Desktop Entry";

fn directory() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(DIR_ENV) {
        return Ok(path.into());
    }
    Ok(dirs::config_dir()
        .context("cannot determine the config directory")?
        .join("autostart"))
}

fn path() -> Result<PathBuf> {
    Ok(directory()?.join(FILE_NAME))
}

pub fn is_enabled() -> bool {
    let Ok(path) = path() else {
        return false;
    };
    let entry = glib::KeyFile::new();
    if entry
        .load_from_file(&path, glib::KeyFileFlags::NONE)
        .is_err()
    {
        return false;
    }
    // Session managers skip entries that other tools hid or disabled.
    let flag = |key| entry.boolean(GROUP, key).ok();
    flag("Hidden") != Some(true) && flag("X-GNOME-Autostart-enabled") != Some(false)
}

pub fn set_enabled(enabled: bool) -> Result<()> {
    let path = path()?;
    if !enabled {
        match std::fs::remove_file(&path) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error).context("cannot remove the autostart entry"),
        }
    }
    std::fs::create_dir_all(path.parent().context("invalid autostart path")?)?;
    std::fs::write(
        path,
        format!(
            "[Desktop Entry]\nType=Application\nName=Services Tray\nComment=Control systemd services from the system tray\nIcon=services-tray\nExec={}\nTerminal=false\nX-GNOME-Autostart-enabled=true\nX-GNOME-Autostart-Delay=3\n",
            executable()
        ),
    )?;
    Ok(())
}

fn executable() -> String {
    match std::env::current_exe() {
        Ok(path) => exec_for(&without_deleted_suffix(path)),
        Err(_) => COMMAND.into(),
    }
}

/// Linux names a binary that was replaced while running, for example by a
/// rebuild or a package upgrade, "<path> (deleted)".
fn without_deleted_suffix(path: PathBuf) -> PathBuf {
    match path.as_os_str().as_bytes().strip_suffix(b" (deleted)") {
        Some(original) if !path.exists() => OsStr::from_bytes(original).into(),
        _ => path,
    }
}

fn exec_for(path: &Path) -> String {
    // Other locations, ~/.local/bin included, are not on every login PATH.
    let system = path.parent().is_some_and(|parent| {
        parent == Path::new("/usr/bin") || parent == Path::new("/usr/local/bin")
    });
    match path.to_str() {
        Some(path) if !system => exec_argument(path),
        _ => COMMAND.into(),
    }
}

/// Escapes one `Exec=` argument as the Desktop Entry specification requires:
/// `%` doubling, then the Exec quoting rule, then the key file string escapes.
fn exec_argument(argument: &str) -> String {
    const RESERVED: &[char] = &[
        ' ', '\t', '\n', '"', '\'', '\\', '>', '<', '~', '|', '&', ';', '$', '*', '?', '#', '(',
        ')', '`',
    ];
    let argument = argument.replace('%', "%%");
    let argument = if argument.contains(RESERVED) {
        let mut quoted = String::from('"');
        for character in argument.chars() {
            if matches!(character, '"' | '`' | '$' | '\\') {
                quoted.push('\\');
            }
            quoted.push(character);
        }
        quoted.push('"');
        quoted
    } else {
        argument
    };
    argument
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
        .replace('\r', "\\r")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn autostart_roundtrip() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("services-tray-test-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::env::set_var(DIR_ENV, &dir);
        assert!(!is_enabled());
        set_enabled(true).unwrap();
        assert!(is_enabled());
        let contents = std::fs::read_to_string(dir.join(FILE_NAME)).unwrap();
        assert!(contents.contains("Name=Services Tray"));
        set_enabled(false).unwrap();
        assert!(!is_enabled());
        std::env::remove_var(DIR_ENV);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn hidden_or_disabled_entries_are_off() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir =
            std::env::temp_dir().join(format!("services-tray-test-hidden-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var(DIR_ENV, &dir);
        for (line, enabled) in [
            ("", true),
            ("Hidden=true", false),
            ("X-GNOME-Autostart-enabled=false", false),
        ] {
            std::fs::write(
                dir.join(FILE_NAME),
                format!("[Desktop Entry]\nType=Application\nExec=services-tray\n{line}\n"),
            )
            .unwrap();
            assert_eq!(is_enabled(), enabled, "{line}");
        }
        std::env::remove_var(DIR_ENV);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn standard_paths_use_command_name() {
        assert_eq!(
            exec_for(Path::new("/usr/bin/services-tray")),
            "services-tray"
        );
        assert_eq!(
            exec_for(Path::new("/usr/local/bin/services-tray")),
            "services-tray"
        );
        assert_eq!(
            exec_for(Path::new("/home/me/.local/bin/services-tray")),
            "/home/me/.local/bin/services-tray"
        );
        assert_eq!(
            exec_for(Path::new("/tmp/my build/services-tray")),
            "\"/tmp/my build/services-tray\""
        );
    }

    #[test]
    fn exec_lines_parse_back_to_the_same_path() {
        for path in [
            "/tmp/my build/services-tray",
            "/tmp/50%/services-tray",
            "/tmp/a\"b$c\\d`e'f/services-tray",
        ] {
            let entry = glib::KeyFile::new();
            let data = format!("[{GROUP}]\nExec={}\n", exec_for(Path::new(path)));
            entry
                .load_from_data(&data, glib::KeyFileFlags::NONE)
                .unwrap();
            // GLib expands field codes such as %% before splitting the line.
            let exec = entry.string(GROUP, "Exec").unwrap().replace("%%", "%");
            let argv = glib::shell_parse_argv(exec.as_str()).unwrap();
            assert_eq!(argv, [std::ffi::OsString::from(path)], "{data}");
        }
    }

    #[test]
    fn replaced_binaries_keep_their_original_path() {
        assert_eq!(
            without_deleted_suffix("/nonexistent/services-tray (deleted)".into()),
            Path::new("/nonexistent/services-tray")
        );
        assert_eq!(
            without_deleted_suffix("/usr/bin/services-tray".into()),
            Path::new("/usr/bin/services-tray")
        );
    }
}
