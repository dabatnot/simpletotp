use crate::State;
use gtk::prelude::*;
use gtk::{gdk, glib};
use gtk4 as gtk;
use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};
use v4l::{
    Device, FourCC,
    buffer::{Flags, Type},
    io::{mmap::Stream, traits::CaptureStream},
    video::Capture,
};

enum Event {
    Code(String),
    Error(String),
}

struct Frame {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
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
    picture.set_content_fit(gtk::ContentFit::Contain);
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
    let (frame_tx, frame_rx) = mpsc::sync_channel(1);
    let (event_tx, event_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = (|| -> anyhow::Result<()> {
            let device = Device::new(0)?;
            let mut format = device.format()?;
            format.width = 640;
            format.height = 480;
            format.fourcc = FourCC::new(b"MJPG");
            let format = device.set_format(&format)?;
            if format.fourcc != FourCC::new(b"MJPG") {
                anyhow::bail!("La caméra ne propose pas le format MJPEG.");
            }
            device.set_params(&v4l::video::capture::Parameters::with_fps(15))?;
            let mut stream = Stream::with_buffers(&device, Type::VideoCapture, 4)?;
            let mut seen = HashSet::new();
            let mut bad_frames = 0;
            while !stop.load(Ordering::Relaxed) {
                let decoded = stream.next().ok().and_then(|(buffer, meta)| {
                    if meta.flags.contains(Flags::ERROR) {
                        return None;
                    }
                    let bytes = buffer.get(..meta.bytesused as usize)?;
                    if !complete_jpeg(bytes) {
                        return None;
                    }
                    image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg)
                        .ok()
                        .map(|image| image.to_rgba8())
                });
                let mut image = match decoded {
                    Some(image) => {
                        bad_frames = 0;
                        image
                    }
                    None => {
                        bad_frames += 1;
                        if bad_frames >= 30 {
                            anyhow::bail!("La caméra ne fournit pas d'image lisible.");
                        }
                        continue;
                    }
                };
                let (width, height) = image.dimensions();
                let gray = image::imageops::grayscale(&image);
                image::imageops::flip_horizontal_in_place(&mut image);
                let _ = frame_tx.try_send(Frame {
                    width,
                    height,
                    pixels: image.into_raw(),
                });
                let mut prepared = rqrr::PreparedImage::prepare(gray);
                for grid in prepared.detect_grids() {
                    if let Ok((_, value)) = grid.decode() {
                        if seen.insert(value.clone()) {
                            let _ = event_tx.send(Event::Code(value));
                        }
                    }
                }
            }
            Ok(())
        })();
        if let Err(err) = result {
            let _ = event_tx.send(Event::Error(err.to_string()));
        }
    });
    let ui = ui.clone();
    let window_for_timer = window.clone();
    glib::timeout_add_local(Duration::from_millis(100), move || {
        if !window_for_timer.is_visible() {
            return glib::ControlFlow::Break;
        }
        if let Ok(frame) = frame_rx.try_recv() {
            let bytes = glib::Bytes::from_owned(frame.pixels);
            let texture = gdk::MemoryTexture::new(
                frame.width as i32,
                frame.height as i32,
                gdk::MemoryFormat::R8g8b8a8,
                &bytes,
                frame.width as usize * 4,
            );
            picture.set_paintable(Some(&texture));
        }
        while let Ok(event) = event_rx.try_recv() {
            match event {
                Event::Code(value) => import(&ui, &value),
                Event::Error(err) => {
                    eprintln!("SimpleTOTP : {err}");
                    report(
                        &ui,
                        "Caméra indisponible. Vous pouvez importer une image.".into(),
                    );
                    window_for_timer.close();
                    return glib::ControlFlow::Break;
                }
            }
        }
        glib::ControlFlow::Continue
    });
}

fn complete_jpeg(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xff, 0xd8]) && bytes.ends_with(&[0xff, 0xd9])
}

#[cfg(test)]
mod tests {
    use super::complete_jpeg;
    use v4l::{
        Device, FourCC,
        buffer::Type,
        io::{mmap::Stream, traits::CaptureStream},
        video::Capture,
    };

    #[test]
    fn rejects_truncated_jpeg_frames() {
        assert!(complete_jpeg(&[0xff, 0xd8, 1, 2, 0xff, 0xd9]));
        assert!(!complete_jpeg(&[0xff, 0xd8, 1, 2]));
    }

    #[test]
    #[ignore = "needs a connected webcam"]
    fn live_webcam_provides_decodable_frames() {
        let device = Device::new(0).unwrap();
        let mut format = device.format().unwrap();
        format.width = 640;
        format.height = 480;
        format.fourcc = FourCC::new(b"MJPG");
        let format = device.set_format(&format).unwrap();
        device
            .set_params(&v4l::video::capture::Parameters::with_fps(15))
            .unwrap();
        assert_eq!(format.fourcc, FourCC::new(b"MJPG"));
        let mut stream = Stream::with_buffers(&device, Type::VideoCapture, 4).unwrap();
        let mut flagged = 0;
        let mut decoded = 0;
        let usable = (0..100)
            .filter(|_| {
                stream.next().is_ok_and(|(buffer, meta)| {
                    if meta.flags.contains(v4l::buffer::Flags::ERROR) {
                        flagged += 1;
                    }
                    let Some(bytes) = buffer.get(..meta.bytesused as usize) else {
                        return false;
                    };
                    let decodable =
                        image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg)
                            .is_ok();
                    if decodable {
                        decoded += 1;
                    }
                    !meta.flags.contains(v4l::buffer::Flags::ERROR)
                        && complete_jpeg(bytes)
                        && decodable
                })
            })
            .count();
        eprintln!(
            "camera: {usable}/100 complete, {decoded}/100 decoded, {flagged}/100 flagged as corrupted"
        );
        assert!(
            usable >= 75,
            "only {usable} of 100 webcam frames were usable"
        );
    }
}
