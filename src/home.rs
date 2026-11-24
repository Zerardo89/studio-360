use std::{
    net::{Ipv4Addr, SocketAddrV4, TcpStream},
    path::PathBuf,
    time::Duration,
};

use eframe::egui::{self, RichText, Ui};

use crate::{
    album::{Album, Asset, Source},
    app::{Job, Tab},
    theme,
};

enum Connection {
    Wifi,
    Usb(char),
}

#[derive(Default)]
pub struct Home {
    connection: Option<Connection>,
    wifi: Option<Job<Result<(), String>>>,
    usb: Option<Job<Option<(char, PathBuf)>>>,
    local: Option<Job<Option<PathBuf>>>,
    wifi_error: Option<String>,
    usb_missing: bool,
}

pub enum Action {
    Go(Tab),
    Open(Asset),
}

impl Home {
    pub fn poll(&mut self) -> [Option<(PathBuf, Source)>; 2] {
        if let Some(result) = Job::take(&mut self.wifi) {
            match result {
                Ok(()) => self.connection = Some(Connection::Wifi),
                Err(e) => {
                    if matches!(self.connection, Some(Connection::Wifi)) {
                        self.connection = None;
                    }
                    self.wifi_error = Some(e);
                }
            }
        }
        let camera = match Job::take(&mut self.usb) {
            Some(Some((letter, folder))) => {
                self.connection = Some(Connection::Usb(letter));
                Some((folder, Source::Camera))
            }
            Some(None) => {
                if matches!(self.connection, Some(Connection::Usb(_))) {
                    self.connection = None;
                }
                self.usb_missing = true;
                None
            }
            None => None,
        };
        let local = Job::take(&mut self.local)
            .flatten()
            .map(|path| (path, Source::Local));
        [camera, local]
    }

    pub fn show(&mut self, ui: &mut Ui, album: &mut Album) -> Option<Action> {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                theme::title(
                    ui,
                    "Home",
                    "Collega una fotocamera o apri una cartella per iniziare.",
                );
                ui.add_space(5.0);
                self.device_card(ui);
                ui.add_space(20.0);
                theme::heading(ui, "Scorciatoie");
                let action = self.shortcuts(ui, album);
                ui.add_space(20.0);
                theme::heading(ui, "Recenti");
                let selected = album.recent(ui).map(Action::Open);
                action.or(selected)
            })
            .inner
    }

    fn device_card(&mut self, ui: &mut Ui) {
        theme::card(20).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            theme::muted(ui, "Fotocamera");
            let title = match self.connection {
                Some(Connection::Wifi) => "Connessa via Wi-Fi".to_owned(),
                Some(Connection::Usb(letter)) => format!("Connessa via USB ({letter}:)"),
                None => "Nessuna fotocamera connessa".to_owned(),
            };
            ui.horizontal(|ui| {
                let (dot, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                ui.painter().circle_filled(
                    dot.center(),
                    4.0,
                    if self.connection.is_some() {
                        theme::OK
                    } else {
                        theme::MUTED
                    },
                );
                ui.label(RichText::new(title).size(23.0).strong());
            });
            ui.add_space(10.0);
            ui.horizontal_wrapped(|ui| {
                let usb = if self.usb.is_some() {
                    "Ricerca USB…"
                } else {
                    "Collega via USB"
                };
                if ui
                    .add_enabled(self.usb.is_none(), theme::primary(usb))
                    .clicked()
                {
                    self.usb_missing = false;
                    self.usb = Some(Job::start(ui.ctx(), find_usb));
                }
                let wifi = if self.wifi.is_some() {
                    "Tentativo Wi-Fi…"
                } else {
                    "Collega via Wi-Fi"
                };
                if ui
                    .add_enabled(self.wifi.is_none(), egui::Button::new(wifi))
                    .clicked()
                {
                    self.wifi_error = None;
                    self.wifi = Some(Job::start(ui.ctx(), connect_wifi));
                }
                if ui
                    .add_enabled(
                        self.local.is_none(),
                        egui::Button::new("Apri cartella locale…"),
                    )
                    .clicked()
                {
                    self.local = Some(Job::start(ui.ctx(), || {
                        rfd::FileDialog::new().pick_folder()
                    }));
                }
            });
            if self.usb_missing {
                ui.colored_label(theme::ERROR, "Nessuna fotocamera USB trovata");
            }
            if let Some(error) = &self.wifi_error {
                ui.colored_label(theme::ERROR, error);
            }
        });
    }

    fn shortcuts(&self, ui: &mut Ui, album: &Album) -> Option<Action> {
        let latest = album.latest();
        let mut action = None;
        ui.horizontal(|ui| {
            let width = (ui.available_width() - 2.0 * ui.spacing().item_spacing.x) / 3.0;
            for (label, tab) in [
                ("Apri album", Tab::Album),
                ("Nuovo scatto", Tab::Scatto),
                ("Modifica ultimo", Tab::Modifica),
            ] {
                let button = egui::Button::new(label).min_size(egui::vec2(width, 48.0));
                if ui
                    .add_enabled(tab != Tab::Modifica || latest.is_some(), button)
                    .clicked()
                {
                    action = if tab == Tab::Modifica {
                        latest.clone().map(Action::Open)
                    } else {
                        Some(Action::Go(tab))
                    };
                }
            }
        });
        action
    }
}

fn connect_wifi() -> Result<(), String> {
    let address = SocketAddrV4::new(Ipv4Addr::new(192, 168, 42, 1), 6666);
    TcpStream::connect_timeout(&address.into(), Duration::from_secs(3))
        .map(|_| ())
        .map_err(|e| format!("Connessione Wi-Fi non riuscita: {e}"))
}

fn find_usb() -> Option<(char, PathBuf)> {
    (b'D'..=b'Z').find_map(|letter| {
        let letter = char::from(letter);
        let root = format!("{letter}:\\");
        if !is_removable(&root) {
            return None;
        }
        let dcim = PathBuf::from(root).join("DCIM");
        dcim.is_dir().then_some((letter, dcim))
    })
}

#[cfg(windows)]
fn is_removable(path: &str) -> bool {
    const DRIVE_REMOVABLE: u32 = 2;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetDriveTypeW(root_path: *const u16) -> u32;
    }
    let root: Vec<_> = path.encode_utf16().chain(std::iter::once(0)).collect();
    // Windows expects a null-terminated UTF-16 root such as D:\.
    unsafe { GetDriveTypeW(root.as_ptr()) == DRIVE_REMOVABLE }
}

#[cfg(not(windows))]
fn is_removable(_path: &str) -> bool {
    false
}
