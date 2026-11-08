use crate::State;
use gtk::prelude::*;
use gtk::{gdk, glib};
use gtk4 as gtk;
use nokhwa::{
    Camera,
    pixel_format::RgbFormat,
    utils::{CameraIndex, RequestedFormat, RequestedFormatType},
};
use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

enum Event {
    Frame(u32, u32, Vec<u8>),
    Code(String),
    Error(String),
}

pub fn scan(ui: &State, import: fn(&State, &str), report: fn(&State, String)) {
    let parent = ui.borrow().window.clone();
    let window = gtk::Window::builder()
        .title("Scanner un QR code")
        .transient_for(&parent)
        .default_width(640)
        .default_height(500)
        .build();
    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 8);
    box_.set_margin_top(12);
    box_.set_margin_bottom(12);
    box_.set_margin_start(12);
    box_.set_margin_end(12);
    let picture = gtk::Picture::new();
    picture.set_hexpand(true);
    picture.set_vexpand(true);
    box_.append(&picture);
    box_.append(&gtk::Label::new(Some(
        "Présentez le QR code du téléphone devant la caméra.",
    )));
    window.set_child(Some(&box_));
    window.present();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_on_close = stop.clone();
    window.connect_close_request(move |_| {
        stop_on_close.store(true, Ordering::Relaxed);
        glib::Propagation::Proceed
    });
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = (|| -> anyhow::Result<()> {
            let format =
                RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestFrameRate);
            let mut camera = Camera::new(CameraIndex::Index(0), format)?;
            camera.open_stream()?;
            let mut seen = HashSet::new();
            while !stop.load(Ordering::Relaxed) {
                let image = camera.frame()?.decode_image::<RgbFormat>()?;
                let (width, height) = image.dimensions();
                let rgb = image.into_raw();
                let _ = tx.send(Event::Frame(width, height, rgb.clone()));
                let gray = image::RgbImage::from_raw(width, height, rgb)
                    .ok_or_else(|| anyhow::anyhow!("Image caméra invalide"))?;
                let mut prepared =
                    rqrr::PreparedImage::prepare(image::DynamicImage::ImageRgb8(gray).to_luma8());
                for grid in prepared.detect_grids() {
                    if let Ok((_, value)) = grid.decode() {
                        if seen.insert(value.clone()) {
                            let _ = tx.send(Event::Code(value));
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(150));
            }
            Ok(())
        })();
        if let Err(err) = result {
            let _ = tx.send(Event::Error(err.to_string()));
        }
    });
    let ui = ui.clone();
    let window_for_timer = window.clone();
    glib::timeout_add_local(Duration::from_millis(100), move || {
        if !window_for_timer.is_visible() {
            return glib::ControlFlow::Break;
        }
        while let Ok(event) = rx.try_recv() {
            match event {
                Event::Frame(width, height, rgb) => {
                    let bytes = glib::Bytes::from_owned(rgb);
                    let texture = gdk::MemoryTexture::new(
                        width as i32,
                        height as i32,
                        gdk::MemoryFormat::R8g8b8,
                        &bytes,
                        width as usize * 3,
                    );
                    picture.set_paintable(Some(&texture));
                }
                Event::Code(value) => import(&ui, &value),
                Event::Error(err) => {
                    report(
                        &ui,
                        format!("Caméra indisponible : {err}. Utilisez l’import par image."),
                    );
                    window_for_timer.close();
                    return glib::ControlFlow::Break;
                }
            }
        }
        glib::ControlFlow::Continue
    });
}
