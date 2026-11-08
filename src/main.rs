mod camera;
mod otp;
mod store;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use gtk4 as gtk;
use ksni::{MenuItem, Tray, blocking::TrayMethods, menu::StandardItem};
use otp::{Account, ImportResult, ImportSession};
use std::{cell::RefCell, rc::Rc, sync::mpsc, time::Duration};
use uuid::Uuid;

enum Action {
    Copy(Uuid),
    Show,
    Quit,
}
struct AppTray {
    accounts: Vec<(Uuid, String)>,
    sender: mpsc::Sender<Action>,
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
    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut menu = Vec::new();
        for (id, label) in &self.accounts {
            let id = *id;
            let tx = self.sender.clone();
            menu.push(
                StandardItem {
                    label: label.replace('_', "__"),
                    activate: Box::new(move |_| {
                        let _ = tx.send(Action::Copy(id));
                    }),
                    ..Default::default()
                }
                .into(),
            );
        }
        if menu.is_empty() {
            menu.push(
                StandardItem {
                    label: "Aucun compte".into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
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
        .map(|a| (a.id, a.label.clone()))
        .collect();
    {
        let state = ui.borrow();
        while let Some(child) = state.list.first_child() {
            state.list.remove(&child);
        }
        for (id, label) in &items {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            row.set_margin_top(6);
            row.set_margin_bottom(6);
            row.set_margin_start(8);
            row.set_margin_end(8);
            let title = gtk::Label::new(Some(label));
            title.set_xalign(0.0);
            title.set_hexpand(true);
            title.set_ellipsize(gtk::pango::EllipsizeMode::End);
            row.append(&title);
            for (text, action) in [("Copier", 0), ("QR", 1), ("Renommer", 2), ("Supprimer", 3)] {
                let button = gtk::Button::with_label(text);
                let ui = ui.clone();
                let id = *id;
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
    }
    if let Some(tray) = &ui.borrow().tray {
        tray.update(|tray| tray.accounts = items);
    }
}

fn copy(ui: &State, id: Uuid) {
    let code = ui
        .borrow()
        .accounts
        .iter()
        .find(|a| a.id == id)
        .map(Account::code);
    if let (Some(code), Some(display)) = (code, gdk::Display::default()) {
        display.clipboard().set_text(&code);
        message(ui, "Code copié.");
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

fn show_qr(ui: &State, id: Uuid) {
    let account = ui.borrow().accounts.iter().find(|a| a.id == id).cloned();
    let Some(account) = account else {
        return;
    };
    let texture = account.qr_png().and_then(|png| {
        let loader = gtk::gdk_pixbuf::PixbufLoader::new();
        loader.write(&png)?;
        loader.close()?;
        let pixbuf = loader
            .pixbuf()
            .ok_or_else(|| anyhow::anyhow!("Image QR illisible"))?;
        Ok(gdk::Texture::for_pixbuf(&pixbuf))
    });
    match texture {
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
    actions.append(&add_button);
    actions.append(&image_button);
    actions.append(&camera_button);
    root.append(&actions);
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
        accounts: accounts.iter().map(|a| (a.id, a.label.clone())).collect(),
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
