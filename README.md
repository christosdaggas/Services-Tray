# Services Tray

![Services Tray: control Linux services from the tray menu](docs/app.png)

Made for **Fedora** with the **GNOME** desktop (tested on Fedora 44).

Services Tray puts your systemd services in the system tray. Click the icon,
pick a service, and start, stop or restart it. You can also add links to a
service, such as its web dashboard, and open them from the same menu.

## What it does

- Shows each service with a coloured dot: 🟢 running, 🟠 starting or
  stopping, 🔴 stopped, ⚪ unknown.
- Starts, stops and restarts user and system services. System services may
  ask for your password.
- Opens a service's links in your browser, for example *Dashboard* →
  `http://127.0.0.1:9001`.
- Exports your list to a file and imports it again, so you can restore it
  after reinstalling.
- Can start automatically when you log in.

## Install

1. GNOME shows tray icons only with the AppIndicator extension. Install it,
   then turn it on in the Extensions app:

   ```sh
   sudo dnf install gnome-shell-extension-appindicator
   ```

2. Download the RPM from the
   [latest release](https://github.com/christosdaggas/Services-Tray/releases/latest)
   and install it:

   ```sh
   sudo dnf install ./services-tray-0.3.0-1.x86_64.rpm
   ```

3. Open **Services Tray** from the app grid.

Other desktops with a system tray, such as KDE Plasma or Xfce, may work but
are not tested.

## How to use

- **Add a service:** tray icon → **Add Service…** → choose *User* or
  *System*, pick the service from the list (or type its name) and click
  **Add**.
- **Control it:** tray icon → the service → **Start**, **Restart** or
  **Stop**. Only the actions that make sense right now can be clicked.
- **Add a link:** tray icon → **Manage Services…** → the pencil next to the
  service → **Add Link**. Enter a title and an address, like `127.0.0.1:80`,
  and click **Save**. The link appears below Start, Restart and Stop.
- **Start at login:** tray icon → **Start tray on login**.

## Back up and restore

1. Before reinstalling: **Manage Services…** → menu (☰) → **Export
   Services…**, and keep the file somewhere safe.
2. After reinstalling: install Services Tray, open **Manage Services…** and
   click **Import Services…**.

If a service is already in the list with different settings, Services Tray
asks whether to replace it or keep it.

## Settings file

Your list is saved in `~/.config/services-tray/services.json`. You can edit it
by hand; restart Services Tray afterwards.

## Build

With Rust installed:

```sh
sudo dnf install gtk4-devel libadwaita-devel
cargo install cargo-generate-rpm    # only once
cargo build --release
cargo generate-rpm -o dist/rpm/
```

The package is created in `dist/rpm/`.

## Troubleshooting

- **No icon in the panel:** no tray is running. On GNOME, turn on the
  AppIndicator extension.
- **It does not start at login:** check that **Start tray on login** has a ✔.
  To see why it failed, run `journalctl --user -b | grep -i services-tray`.

## License

MIT, see [LICENSE](LICENSE).
