//! Ventana que aparece cuando llega un recordatorio, con su contenido, la alarma y
//! los botones de hecho y posponer.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::glib;

use crate::app::Command;
use crate::attachment_view::attachments_widget;
use crate::audio::{self, Sound};
use crate::conversation_view::markdown_label;
use crate::explorer::local_time;
use crate::i18n::tr;
use crate::store::Reminder;

/// Cada cuánto suena la alarma y durante cuánto como mucho.
const ALARM_EVERY: Duration = Duration::from_secs(6);
const ALARM_FOR: Duration = Duration::from_secs(120);

pub struct ReminderWindow {
    window: adw::Window,
    /// El temporizador de la alarma lo mira en cada vuelta (quitarlo a mano fallaría
    /// si ya ha terminado solo).
    alarm_on: Rc<Cell<bool>>,
}

impl ReminderWindow {
    /// `snooze`: opciones (valor, etiqueta) para posponer.
    pub fn show(
        app: &adw::Application,
        reminder: &Reminder,
        snooze: &[(String, String)],
        commands: async_channel::Sender<Command>,
        alarm: Option<bool>,
        activation_token: Option<&str>,
    ) -> Rc<Self> {
        let title = gtk::Label::builder().label(&reminder.title).wrap(true).xalign(0.0).css_classes(["title-1"]).build();
        let when = gtk::Label::builder().label(local_time(reminder.due_at)).xalign(0.0).css_classes(["dim-label"]).build();
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(18)
            .margin_end(18)
            .build();
        content.append(&title);
        content.append(&when);
        if !reminder.body.trim().is_empty() {
            content.append(&markdown_label(&reminder.body));
        }
        if !reminder.attachments.is_empty() {
            content.append(&attachments_widget(&reminder.attachments, None));
        }
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(560)
            .vexpand(true)
            .child(&content)
            .build();

        let snooze_list = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).build();
        let snooze_popover = gtk::Popover::builder().child(&snooze_list).build();
        let snooze_button = gtk::MenuButton::builder().label(tr("Posponer")).popover(&snooze_popover).build();
        let done = gtk::Button::builder().label(tr("Hecho")).css_classes(["suggested-action"]).build();
        let actions = gtk::Box::builder()
            .spacing(6)
            .halign(gtk::Align::End)
            .margin_top(6)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        actions.append(&snooze_button);
        actions.append(&done);

        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.append(&scroller);
        body.append(&actions);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        toolbar.set_content(Some(&body));
        let window = adw::Window::builder()
            .application(app)
            .title(tr("Recordatorio"))
            .default_width(460)
            .content(&toolbar)
            .build();

        let this = Rc::new(Self { window, alarm_on: Rc::new(Cell::new(false)) });
        let id = reminder.id;
        for (option, label) in snooze {
            let item = gtk::Button::builder().label(label).css_classes(["flat"]).build();
            let (commands, option) = (commands.clone(), option.clone());
            item.connect_clicked(move |_| {
                let _ = commands.try_send(Command::ReminderSnooze(id, Some(option.clone())));
            });
            snooze_list.append(&item);
        }
        done.connect_clicked(move |_| {
            let _ = commands.try_send(Command::ReminderDone(id));
        });
        // Cerrar la ventana cuenta como atendido: se calla la alarma.
        this.window.connect_close_request(glib::clone!(
            #[weak]
            this,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_| {
                this.stop_alarm();
                glib::Propagation::Proceed
            }
        ));

        if let Some(repeat) = alarm {
            audio::play(Sound::Alarm);
            if repeat {
                let started = Instant::now();
                let on = this.alarm_on.clone();
                on.set(true);
                glib::timeout_add_local(ALARM_EVERY, move || {
                    if !on.get() || started.elapsed() > ALARM_FOR {
                        return glib::ControlFlow::Break;
                    }
                    audio::play(Sound::Alarm);
                    glib::ControlFlow::Continue
                });
            }
        }
        if let Some(token) = activation_token {
            this.window.set_startup_id(token);
        }
        this.window.present();
        done.grab_focus();
        this
    }

    fn stop_alarm(&self) {
        self.alarm_on.set(false);
    }

    pub fn close(&self) {
        self.stop_alarm();
        self.window.close();
    }

    pub fn window(&self) -> &adw::Window {
        &self.window
    }

    pub fn present(&self) {
        self.window.present();
    }
}
