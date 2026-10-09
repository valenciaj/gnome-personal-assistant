//! Ventanita que se muestra mientras se graba: tiempo, nivel del micrófono y
//! botones para parar o descartar.
//!
//! En Wayland GNOME decide dónde aparece (normalmente centrada) y no puede
//! quedarse por encima del resto de ventanas.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::glib;

use crate::app::Command;

pub struct Indicator {
    window: adw::Window,
    time: gtk::Label,
    level: gtk::LevelBar,
    started: Cell<Instant>,
    timer: RefCell<Option<glib::SourceId>>,
}

impl Indicator {
    pub fn new(app: &adw::Application, commands: async_channel::Sender<Command>) -> Rc<Self> {
        let dot = gtk::Image::builder().icon_name("media-record-symbolic").css_classes(["error"]).build();
        let time = gtk::Label::builder().label("0:00").css_classes(["numeric", "title-4"]).build();
        let level = gtk::LevelBar::builder().min_value(0.0).max_value(1.0).hexpand(true).valign(gtk::Align::Center).build();
        // Sin los tramos de color por defecto (pensados para baterías).
        for name in [gtk::LEVEL_BAR_OFFSET_LOW, gtk::LEVEL_BAR_OFFSET_HIGH, gtk::LEVEL_BAR_OFFSET_FULL] {
            level.remove_offset_value(Some(name));
        }
        let cancel = gtk::Button::builder()
            .icon_name("edit-delete-symbolic")
            .tooltip_text("Descartar")
            .css_classes(["circular", "flat"])
            .valign(gtk::Align::Center)
            .build();
        let stop = gtk::Button::builder()
            .icon_name("media-playback-stop-symbolic")
            .tooltip_text("Parar y enviar")
            .css_classes(["circular", "destructive-action"])
            .valign(gtk::Align::Center)
            .build();

        let content = gtk::Box::builder()
            .spacing(12)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(16)
            .margin_end(12)
            .build();
        content.append(&dot);
        content.append(&time);
        content.append(&level);
        content.append(&cancel);
        content.append(&stop);

        let handle = gtk::WindowHandle::builder().child(&content).build();
        let window = adw::Window::builder()
            .application(app)
            .title("Grabando")
            .default_width(380)
            // AdwWindow impone por defecto un mínimo de 360×200.
            .width_request(300)
            .height_request(56)
            .resizable(false)
            .decorated(false)
            .hide_on_close(true)
            .content(&handle)
            .build();
        window.add_css_class("osd");

        for (button, command) in [(&stop, Command::StopRecording), (&cancel, Command::CancelRecording)] {
            let commands = commands.clone();
            button.connect_clicked(move |_| {
                let _ = commands.try_send(command.clone());
            });
        }
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(move |_, key, _, _| match key {
            gtk::gdk::Key::Escape => {
                let _ = commands.try_send(Command::CancelRecording);
                glib::Propagation::Stop
            }
            gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter | gtk::gdk::Key::space => {
                let _ = commands.try_send(Command::StopRecording);
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        });
        window.add_controller(keys);

        Rc::new(Self { window, time, level, started: Cell::new(Instant::now()), timer: RefCell::new(None) })
    }

    pub fn show(self: &Rc<Self>, activation_token: Option<&str>) {
        self.started.set(Instant::now());
        self.time.set_text("0:00");
        self.level.set_value(0.0);
        let weak = Rc::downgrade(self);
        let timer = glib::timeout_add_local(Duration::from_millis(250), move || {
            let Some(this) = weak.upgrade() else { return glib::ControlFlow::Break };
            let seconds = this.started.get().elapsed().as_secs();
            this.time.set_text(&format!("{}:{:02}", seconds / 60, seconds % 60));
            glib::ControlFlow::Continue
        });
        if let Some(old) = self.timer.replace(Some(timer)) {
            old.remove();
        }
        if let Some(token) = activation_token {
            self.window.set_startup_id(token);
        }
        self.window.present();
    }

    pub fn window(&self) -> &adw::Window {
        &self.window
    }

    pub fn hide(&self) {
        if let Some(timer) = self.timer.take() {
            timer.remove();
        }
        self.window.set_visible(false);
    }

    /// Nivel RMS en dBFS: -60 dB o menos es silencio, 0 dB el máximo.
    pub fn set_level(&self, db: f64) {
        self.level.set_value(((db + 60.0) / 60.0).clamp(0.0, 1.0));
    }
}
