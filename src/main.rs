mod camera;
mod otp;
mod store;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use gtk4 as gtk;
use image::ImageEncoder;
use ksni::{
    MenuItem, Tray,
    blocking::TrayMethods,
    menu::{StandardItem, SubMenu},
};
use otp::{Account, ImportResult, ImportSession};
use std::{
    cell::{Cell, RefCell},
    io::Write,
    process::{Command, Stdio},
    rc::Rc,
    sync::mpsc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

enum Action {
    Copy(Uuid),
    Show,
    Quit,
}
struct AppTray {
    accounts: Vec<TrayAccount>,
    sender: mpsc::Sender<Action>,
}

#[derive(Clone)]
struct TrayAccount {
    id: Uuid,
    label: String,
    period: u64,
    favorite: bool,
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn seconds_left(period: u64, now: u64) -> u64 {
    let period = period.max(1);
    period - now % period
}

fn freshness_icon(remaining: u64, period: u64) -> Vec<u8> {
    let mut image = image::RgbaImage::new(32, 32);
    let progress = remaining as f64 / period.max(1) as f64;
    let active = if remaining <= 5 {
        [220, 75, 65, 255]
    } else if remaining <= 10 {
        [225, 165, 55, 255]
    } else {
        [65, 170, 90, 255]
    };
    for y in 0..32 {
        for x in 0..32 {
            let dx = x as f64 - 15.5;
            let dy = y as f64 - 15.5;
            let radius = (dx * dx + dy * dy).sqrt();
            if (9.0..=13.0).contains(&radius) {
                let angle =
                    (dy.atan2(dx) + std::f64::consts::FRAC_PI_2).rem_euclid(std::f64::consts::TAU);
                let color = if angle / std::f64::consts::TAU < progress {
                    active
                } else {
                    [130, 130, 130, 120]
                };
                image.put_pixel(x, y, image::Rgba(color));
            }
        }
    }
    let mut png = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut png);
    if encoder
        .write_image(image.as_raw(), 32, 32, image::ExtendedColorType::Rgba8)
        .is_err()
    {
        return Vec::new();
    }
    png
}

impl AppTray {
    fn account_item(&self, account: &TrayAccount, now: u64) -> MenuItem<Self> {
        let id = account.id;
        let tx = self.sender.clone();
        let remaining = seconds_left(account.period, now);
        StandardItem {
            label: format!("{} · {remaining} s", account.label.replace('_', "__")),
            enabled: remaining > 2,
            icon_data: freshness_icon(remaining, account.period),
            activate: Box::new(move |_| {
                let _ = tx.send(Action::Copy(id));
            }),
            ..Default::default()
        }
        .into()
    }
}
impl Tray for AppTray {
    const MENU_ON_ACTIVATE: bool = true;
    fn id(&self) -> String {
        "simpletotp".into()
    }
    fn title(&self) -> String {
        "SimpleTOTP".into()
    }
    fn icon_name(&self) -> String {
        "dialog-password-symbolic".into()
    }
    fn menu_about_to_show(&mut self) {}
    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut menu = Vec::new();
        let now = now_seconds();
        for account in self.accounts.iter().filter(|a| a.favorite).take(10) {
            menu.push(self.account_item(account, now));
        }
        if !menu.is_empty() {
            menu.push(MenuItem::Separator);
        }
        if self.accounts.is_empty() {
            menu.push(
                StandardItem {
                    label: "Aucun compte".into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        } else {
            for (index, page) in self.accounts.chunks(10).enumerate() {
                let start = index * 10 + 1;
                let end = start + page.len() - 1;
                menu.push(
                    SubMenu {
                        label: if self.accounts.len() <= 10 {
                            "Tous les codes".into()
                        } else {
                            format!("Tous les codes · {start}–{end}")
                        },
                        submenu: page
                            .iter()
                            .map(|account| self.account_item(account, now))
                            .collect(),
                        ..Default::default()
                    }
                    .into(),
                );
            }
        }
        menu.push(MenuItem::Separator);
        let tx = self.sender.clone();
        menu.push(
            StandardItem {
                label: "Gérer les comptes".into(),
                activate: Box::new(move |_| {
                    let _ = tx.send(Action::Show);
                }),
                ..Default::default()
            }
            .into(),
        );
        let tx = self.sender.clone();
        menu.push(
            StandardItem {
                label: "Quitter".into(),
                activate: Box::new(move |_| {
                    let _ = tx.send(Action::Quit);
                }),
                ..Default::default()
            }
            .into(),
        );
        menu
    }
}

struct Ui {
    window: gtk::ApplicationWindow,
    search: gtk::SearchEntry,
    list: gtk::ListBox,
    status: gtk::Label,
    accounts: Vec<Account>,
    tray: Option<ksni::blocking::Handle<AppTray>>,
    importer: ImportSession,
    _hold: gio::ApplicationHoldGuard,
}
type State = Rc<RefCell<Ui>>;
fn message(ui: &State, text: impl AsRef<str>) {
    ui.borrow().status.set_text(text.as_ref());
}

fn refresh(ui: &State) {
    let items: Vec<_> = ui
        .borrow()
        .accounts
        .iter()
        .map(|a| TrayAccount {
            id: a.id,
            label: a.label.clone(),
            period: a.totp.step(),
            favorite: a.favorite,
        })
        .collect();
    {
        let state = ui.borrow();
        let query = state.search.text().to_lowercase();
        while let Some(child) = state.list.first_child() {
            state.list.remove(&child);
        }
        let mut shown = 0;
        for item in items
            .iter()
            .filter(|item| item.label.to_lowercase().contains(&query))
        {
            shown += 1;
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            row.set_margin_top(6);
            row.set_margin_bottom(6);
            row.set_margin_start(8);
            row.set_margin_end(8);
            let favorite = gtk::Button::with_label(if item.favorite { "★" } else { "☆" });
            favorite.set_tooltip_text(Some(if item.favorite {
                "Retirer des favoris"
            } else {
                "Ajouter aux favoris"
            }));
            let target = ui.clone();
            let id = item.id;
            favorite.connect_clicked(move |_| toggle_favorite(&target, id));
            row.append(&favorite);
            let title = gtk::Label::new(Some(&item.label));
            title.set_xalign(0.0);
            title.set_hexpand(true);
            title.set_ellipsize(gtk::pango::EllipsizeMode::End);
            row.append(&title);
            for (text, action) in [("Copier", 0), ("QR", 1), ("Renommer", 2), ("Supprimer", 3)] {
                let button = gtk::Button::with_label(text);
                let ui = ui.clone();
                let id = item.id;
                button.connect_clicked(move |_| match action {
                    0 => copy(&ui, id),
                    1 => show_qr(&ui, id),
                    2 => rename(&ui, id),
                    _ => remove(&ui, id),
                });
                row.append(&button);
            }
            state.list.append(&row);
        }
        if shown == 0 {
            state
                .list
                .append(&gtk::Label::new(Some(if query.is_empty() {
                    "Aucun compte enregistré."
                } else {
                    "Aucun compte trouvé."
                })));
        }
    }
    if let Some(tray) = &ui.borrow().tray {
        tray.update(|tray| tray.accounts = items);
    }
}

fn toggle_favorite(ui: &State, id: Uuid) {
    let state = ui.borrow();
    let Some(account) = state.accounts.iter().find(|a| a.id == id) else {
        return;
    };
    if !account.favorite && state.accounts.iter().filter(|a| a.favorite).count() >= 10 {
        drop(state);
        message(ui, "Dix favoris au maximum.");
        return;
    }
    let mut updated = account.clone();
    updated.favorite = !updated.favorite;
    drop(state);
    match store::save(&updated) {
        Ok(()) => {
            if let Some(account) = ui.borrow_mut().accounts.iter_mut().find(|a| a.id == id) {
                *account = updated.clone();
            }
            refresh(ui);
            message(
                ui,
                if updated.favorite {
                    "Favori ajouté."
                } else {
                    "Favori retiré."
                },
            );
        }
        Err(err) => message(ui, format!("Enregistrement impossible : {err}")),
    }
}

fn set_clipboard(code: &str) -> anyhow::Result<()> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        let mut child = Command::new("wl-copy")
            .args(["--type", "text/plain;charset=utf-8"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let mut input = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("Entrée du presse-papiers indisponible"))?;
        input.write_all(code.as_bytes())?;
        drop(input);
        if !child.wait()?.success() {
            anyhow::bail!("wl-copy a échoué");
        }
    } else {
        let display = gdk::Display::default()
            .ok_or_else(|| anyhow::anyhow!("Presse-papiers indisponible"))?;
        display.clipboard().set_text(code);
    }
    Ok(())
}

fn copy(ui: &State, id: Uuid) {
    let now = now_seconds();
    let code = ui
        .borrow()
        .accounts
        .iter()
        .find(|a| a.id == id)
        .and_then(|a| {
            (seconds_left(a.totp.step(), now) > 2).then(|| a.totp.generate(now).to_string())
        });
    if let Some(code) = code {
        match set_clipboard(&code) {
            Ok(()) => message(ui, "Code copié."),
            Err(err) => message(ui, format!("Copie impossible : {err}")),
        }
    }
}

fn save(ui: &State, accounts: Vec<Account>) -> (usize, usize) {
    let mut added = 0;
    let mut failed = 0;
    for account in accounts {
        if ui.borrow().accounts.iter().any(|a| a.totp == account.totp) {
            continue;
        }
        match store::save(&account) {
            Ok(()) => {
                ui.borrow_mut().accounts.push(account);
                added += 1;
            }
            Err(_) => failed += 1,
        }
    }
    ui.borrow_mut()
        .accounts
        .sort_by_key(|a| a.label.to_lowercase());
    refresh(ui);
    message(
        ui,
        if failed == 0 {
            format!("{added} compte(s) ajouté(s).")
        } else {
            format!("{added} compte(s) ajouté(s), {failed} erreur(s) de stockage.")
        },
    );
    (added, failed)
}

fn add(ui: &State) {
    let parent = ui.borrow().window.clone();
    let window = gtk::Window::builder()
        .title("Ajouter un compte")
        .transient_for(&parent)
        .modal(true)
        .default_width(430)
        .build();
    let box_ = column();
    let service = gtk::Entry::builder().placeholder_text("Service").build();
    let name = gtk::Entry::builder().placeholder_text("Compte").build();
    let key = gtk::Entry::builder()
        .placeholder_text("Clé Base32 ou adresse otpauth://")
        .build();
    key.set_visibility(false);
    let error = gtk::Label::new(None);
    error.set_wrap(true);
    let button = gtk::Button::with_label("Ajouter");
    for widget in [&service, &name, &key] {
        box_.append(widget);
    }
    box_.append(&error);
    box_.append(&button);
    window.set_child(Some(&box_));
    let ui = ui.clone();
    let close = window.clone();
    button.connect_clicked(move |_| {
        let raw = key.text();
        let result = if raw.starts_with("otpauth://") {
            Account::from_url(&raw)
        } else {
            Account::from_key(&service.text(), &name.text(), &raw)
        };
        match result {
            Ok(account) => {
                if save(&ui, vec![account]).1 == 0 {
                    close.close();
                }
            }
            Err(err) => error.set_text(&format!("{err:#}")),
        }
    });
    window.present();
}

fn qr_texture(png: &[u8]) -> anyhow::Result<gdk::Texture> {
    let loader = gtk::gdk_pixbuf::PixbufLoader::new();
    loader.write(png)?;
    loader.close()?;
    let pixbuf = loader
        .pixbuf()
        .ok_or_else(|| anyhow::anyhow!("Image QR illisible"))?;
    Ok(gdk::Texture::for_pixbuf(&pixbuf))
}

fn show_qr(ui: &State, id: Uuid) {
    let account = ui.borrow().accounts.iter().find(|a| a.id == id).cloned();
    let Some(account) = account else {
        return;
    };
    match account.qr_png().and_then(|png| qr_texture(&png)) {
        Ok(texture) => {
            let parent = ui.borrow().window.clone();
            let window = gtk::Window::builder()
                .title(format!("Transférer {}", account.label))
                .transient_for(&parent)
                .modal(true)
                .default_width(360)
                .build();
            let box_ = column();
            box_.append(&gtk::Label::new(Some(
                "Scannez ce code avec votre téléphone.",
            )));
            box_.append(&gtk::Picture::for_paintable(&texture));
            let button = gtk::Button::with_label("Fermer");
            let close = window.clone();
            button.connect_clicked(move |_| close.close());
            box_.append(&button);
            window.set_child(Some(&box_));
            window.present();
        }
        Err(err) => message(ui, format!("QR indisponible : {err}")),
    }
}

fn show_qr_series(
    ui: &State,
    title: &str,
    instruction: &str,
    pages: Vec<(String, Vec<u8>)>,
    size: i32,
) {
    let parent = ui.borrow().window.clone();
    let window = gtk::Window::builder()
        .title(title)
        .transient_for(&parent)
        .modal(true)
        .default_width(size + 60)
        .build();
    let box_ = column();
    let progress = gtk::Label::new(None);
    progress.set_wrap(true);
    let picture = gtk::Picture::new();
    picture.set_size_request(size, size);
    picture.set_content_fit(gtk::ContentFit::Contain);
    let instructions = gtk::Label::new(Some(instruction));
    instructions.set_wrap(true);
    let error = gtk::Label::new(None);
    error.set_wrap(true);
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let previous = gtk::Button::with_label("Précédent");
    let next = gtk::Button::with_label("Suivant");
    let close = gtk::Button::with_label("Fermer");
    buttons.append(&previous);
    buttons.append(&next);
    buttons.append(&close);
    box_.append(&progress);
    box_.append(&picture);
    box_.append(&instructions);
    box_.append(&error);
    box_.append(&buttons);
    window.set_child(Some(&box_));

    let current = Rc::new(Cell::new(0usize));
    let previous_for_render = previous.clone();
    let next_for_render = next.clone();
    let render = Rc::new(move |index: usize| {
        progress.set_text(&format!(
            "{} / {} · {}",
            index + 1,
            pages.len(),
            pages[index].0
        ));
        previous_for_render.set_sensitive(index > 0);
        next_for_render.set_sensitive(index + 1 < pages.len());
        match qr_texture(&pages[index].1) {
            Ok(texture) => {
                picture.set_paintable(Some(&texture));
                error.set_text("");
            }
            Err(err) => {
                picture.set_paintable(None::<&gdk::Texture>);
                error.set_text(&format!("QR indisponible : {err}"));
            }
        }
    });
    render(0);
    let index = current.clone();
    let update = render.clone();
    previous.connect_clicked(move |_| {
        let value = index.get() - 1;
        index.set(value);
        update(value);
    });
    let index = current.clone();
    next.connect_clicked(move |_| {
        let value = index.get() + 1;
        index.set(value);
        render(value);
    });
    let dismiss = window.clone();
    close.connect_clicked(move |_| dismiss.close());
    window.present();
}

fn transfer_all(ui: &State) {
    let accounts = ui.borrow().accounts.clone();
    if accounts.is_empty() {
        message(ui, "Aucun compte à transférer.");
        return;
    }
    let pages = accounts
        .iter()
        .map(|account| account.qr_png().map(|png| (account.label.clone(), png)))
        .collect::<anyhow::Result<Vec<_>>>();
    match pages {
        Ok(pages) => show_qr_series(
            ui,
            "Réimporter sur le téléphone",
            "Scannez ce QR avec votre téléphone. Vérifiez que le compte apparaît, puis passez au suivant.",
            pages,
            320,
        ),
        Err(err) => message(ui, format!("QR indisponible : {err}")),
    }
}

fn export_google(ui: &State) {
    let accounts = ui.borrow().accounts.clone();
    let images = otp::google_export_qrs(&accounts);
    match images {
        Ok(images) => {
            let pages = images
                .into_iter()
                .enumerate()
                .map(|(index, png)| {
                    let first = index * otp::GOOGLE_BATCH_LIMIT + 1;
                    let last = (first + otp::GOOGLE_BATCH_LIMIT - 1).min(accounts.len());
                    (format!("Comptes {first} à {last}"), png)
                })
                .collect();
            show_qr_series(
                ui,
                "QR groupés Google Authenticator",
                "Scannez les QR dans l’ordre avec l’import de Google Authenticator. Si ces comptes sont déjà sur le téléphone, l’import peut créer des doublons.",
                pages,
                560,
            );
        }
        Err(err) => message(ui, format!("Export groupé impossible : {err}")),
    }
}

fn rename(ui: &State, id: Uuid) {
    let account = ui.borrow().accounts.iter().find(|a| a.id == id).cloned();
    let Some(account) = account else {
        return;
    };
    let parent = ui.borrow().window.clone();
    let window = gtk::Window::builder()
        .title("Renommer")
        .transient_for(&parent)
        .modal(true)
        .default_width(360)
        .build();
    let box_ = column();
    let entry = gtk::Entry::new();
    entry.set_text(&account.label);
    let button = gtk::Button::with_label("Enregistrer");
    box_.append(&entry);
    box_.append(&button);
    window.set_child(Some(&box_));
    let ui = ui.clone();
    let close = window.clone();
    button.connect_clicked(move |_| {
        let name = entry.text().trim().to_owned();
        if name.is_empty() {
            return;
        }
        let mut updated = account.clone();
        updated.label = name;
        match store::save(&updated) {
            Ok(()) => {
                if let Some(old) = ui.borrow_mut().accounts.iter_mut().find(|a| a.id == id) {
                    *old = updated;
                }
                refresh(&ui);
                close.close();
                message(&ui, "Compte renommé.");
            }
            Err(err) => message(&ui, format!("Enregistrement impossible : {err}")),
        }
    });
    window.present();
}

fn remove(ui: &State, id: Uuid) {
    let label = ui
        .borrow()
        .accounts
        .iter()
        .find(|a| a.id == id)
        .map(|a| a.label.clone());
    let Some(label) = label else {
        return;
    };
    let parent = ui.borrow().window.clone();
    let window = gtk::Window::builder()
        .title("Supprimer le compte")
        .transient_for(&parent)
        .modal(true)
        .default_width(360)
        .build();
    let box_ = column();
    box_.append(&gtk::Label::new(Some(&format!("Supprimer {label} ?"))));
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let cancel = gtk::Button::with_label("Annuler");
    let confirm = gtk::Button::with_label("Supprimer");
    row.append(&cancel);
    row.append(&confirm);
    box_.append(&row);
    window.set_child(Some(&box_));
    let close = window.clone();
    cancel.connect_clicked(move |_| close.close());
    let ui = ui.clone();
    let close = window.clone();
    confirm.connect_clicked(move |_| match store::delete(id) {
        Ok(()) => {
            ui.borrow_mut().accounts.retain(|a| a.id != id);
            refresh(&ui);
            close.close();
            message(&ui, "Compte supprimé.");
        }
        Err(err) => message(&ui, format!("Suppression impossible : {err}")),
    });
    window.present();
}

fn import_text(ui: &State, text: &str) {
    let result = ui.borrow_mut().importer.ingest(text);
    match result {
        Ok(ImportResult::Pending(done, total)) => message(
            ui,
            format!("Export Google : QR {done}/{total}. Importez le suivant."),
        ),
        Ok(ImportResult::Ready(accounts, skipped)) => {
            if accounts.is_empty() {
                message(ui, "Aucun compte TOTP compatible.");
            } else {
                save(ui, accounts);
            }
            if skipped > 0 {
                message(
                    ui,
                    format!("{skipped} compte(s) non TOTP ou incompatibles ignorés."),
                );
            }
        }
        Err(err) => message(ui, format!("Import impossible : {err:#}")),
    }
}

fn import_image(ui: &State) {
    let dialog = gtk::FileDialog::new();
    let parent = ui.borrow().window.clone();
    let ui = ui.clone();
    dialog.open(Some(&parent), None::<&gio::Cancellable>, move |result| {
        let Ok(file) = result else {
            return;
        };
        let Some(path) = file.path() else {
            message(&ui, "Fichier inaccessible.");
            return;
        };
        match std::fs::read(path)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| otp::read_qr_image(&bytes))
        {
            Ok(values) => {
                for value in values {
                    import_text(&ui, &value);
                }
            }
            Err(err) => message(&ui, format!("Import impossible : {err:#}")),
        }
    });
}

fn column() -> gtk::Box {
    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 10);
    box_.set_margin_top(18);
    box_.set_margin_bottom(18);
    box_.set_margin_start(18);
    box_.set_margin_end(18);
    box_
}

fn build(app: &gtk::Application, hidden: bool, hold: gio::ApplicationHoldGuard) -> State {
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("SimpleTOTP")
        .default_width(720)
        .default_height(500)
        .build();
    let root = column();
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let add_button = gtk::Button::with_label("Ajouter");
    let image_button = gtk::Button::with_label("Importer une image");
    let camera_button = gtk::Button::with_label("Scanner avec la caméra");
    let transfer_button = gtk::Button::with_label("Réimporter sur le téléphone");
    let google_button = gtk::Button::with_label("QR groupés Google");
    actions.append(&add_button);
    actions.append(&image_button);
    actions.append(&camera_button);
    root.append(&actions);
    let transfers = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    transfers.append(&transfer_button);
    transfers.append(&google_button);
    root.append(&transfers);
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Rechercher un compte"));
    root.append(&search);
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_child(Some(&list));
    scroll.set_vexpand(true);
    root.append(&scroll);
    let status = gtk::Label::new(None);
    status.set_xalign(0.0);
    status.set_wrap(true);
    root.append(&status);
    window.set_child(Some(&root));
    let accounts = match store::load() {
        Ok(accounts) => accounts,
        Err(err) => {
            status.set_text(&format!("Coffre de secrets indisponible : {err:#}"));
            Vec::new()
        }
    };
    let (sender, receiver) = mpsc::channel();
    let tray = AppTray {
        accounts: accounts
            .iter()
            .map(|a| TrayAccount {
                id: a.id,
                label: a.label.clone(),
                period: a.totp.step(),
                favorite: a.favorite,
            })
            .collect(),
        sender,
    };
    let tray = match tray.spawn() {
        Ok(handle) => Some(handle),
        Err(err) => {
            status.set_text(&format!("Icône indisponible : {err}"));
            None
        }
    };
    let ui = Rc::new(RefCell::new(Ui {
        window: window.clone(),
        search: search.clone(),
        list,
        status,
        accounts,
        tray,
        importer: ImportSession::default(),
        _hold: hold,
    }));
    let target = ui.clone();
    add_button.connect_clicked(move |_| add(&target));
    let target = ui.clone();
    image_button.connect_clicked(move |_| import_image(&target));
    let target = ui.clone();
    camera_button.connect_clicked(move |_| camera::scan(&target, import_text, message));
    let target = ui.clone();
    transfer_button.connect_clicked(move |_| transfer_all(&target));
    let target = ui.clone();
    google_button.connect_clicked(move |_| export_google(&target));
    let target = ui.clone();
    search.connect_search_changed(move |_| refresh(&target));
    window.connect_close_request(|window| {
        window.set_visible(false);
        glib::Propagation::Stop
    });
    let target = ui.clone();
    let app = app.clone();
    glib::timeout_add_local(Duration::from_millis(100), move || {
        while let Ok(action) = receiver.try_recv() {
            match action {
                Action::Copy(id) => copy(&target, id),
                Action::Show => target.borrow().window.present(),
                Action::Quit => {
                    app.quit();
                    return glib::ControlFlow::Break;
                }
            }
        }
        glib::ControlFlow::Continue
    });
    let target = ui.clone();
    glib::timeout_add_local(Duration::from_secs(1), move || {
        if let Some(tray) = &target.borrow().tray {
            tray.update(|_| {});
        }
        glib::ControlFlow::Continue
    });
    refresh(&ui);
    if !hidden || ui.borrow().tray.is_none() {
        window.present();
    }
    ui
}

fn main() {
    let hidden = std::env::args().any(|arg| arg == "--background");
    let app = gtk::Application::builder()
        .application_id("io.github.dabatnot.simpletotp")
        .build();
    let state = Rc::new(RefCell::new(None::<State>));
    app.connect_activate(move |app| {
        if let Some(ui) = state.borrow().as_ref() {
            ui.borrow().window.present();
            return;
        }
        let hold = app.hold();
        *state.borrow_mut() = Some(build(app, hidden, hold));
    });
    app.run();
}

#[cfg(test)]
mod tray_tests {
    use super::{Action, AppTray, TrayAccount, freshness_icon, seconds_left};
    use ksni::{MenuItem, Tray};
    use std::sync::mpsc;
    use uuid::Uuid;

    #[test]
    fn countdown_and_ring_refresh_at_code_boundary() {
        assert_eq!(seconds_left(30, 29), 1);
        assert_eq!(seconds_left(30, 30), 30);
        let ring = image::load_from_memory(&freshness_icon(15, 30)).unwrap();
        assert_eq!((ring.width(), ring.height()), (32, 32));
    }

    #[test]
    fn favorites_then_all_accounts_in_pages_of_ten() {
        let (sender, receiver) = mpsc::channel();
        let mut tray = AppTray {
            accounts: (0..12)
                .map(|index| TrayAccount {
                    id: Uuid::from_u128(index),
                    label: format!("Compte {index}"),
                    period: 30,
                    favorite: index < 11,
                })
                .collect(),
            sender,
        };
        let menu = tray.menu();
        assert!(
            menu[..10]
                .iter()
                .all(|item| matches!(item, MenuItem::Standard(_)))
        );
        assert!(matches!(menu[10], MenuItem::Separator));
        let MenuItem::SubMenu(first_page) = &menu[11] else {
            panic!("missing first page");
        };
        let MenuItem::SubMenu(second_page) = &menu[12] else {
            panic!("missing second page");
        };
        assert_eq!(first_page.submenu.len(), 10);
        assert_eq!(second_page.submenu.len(), 2);
        let MenuItem::Standard(first_account) = &first_page.submenu[0] else {
            panic!("missing first account");
        };
        (first_account.activate)(&mut tray);
        assert!(matches!(receiver.try_recv(), Ok(Action::Copy(id)) if id == Uuid::from_u128(0)));
    }
}
