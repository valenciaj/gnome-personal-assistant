//! Ventana rápida de conversación: transcripción, razonamiento y respuesta.

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::app::Command;
use crate::attachment_view::{AddOptions, add_button, pending_widget};
use crate::conversation_view::{ConversationView, wrapped_label};
use crate::store::NewAttachment;
use crate::i18n::tr;

pub struct Popup {
    window: adw::Window,
    view: ConversationView,
    status: gtk::Box,
    status_label: gtk::Label,
    entry: gtk::Entry,
    mic: gtk::Button,
    send: gtk::Button,
    pending: gtk::Box,
    commands: async_channel::Sender<Command>,
}

impl Popup {
    pub fn new(app: &adw::Application, commands: async_channel::Sender<Command>) -> Self {
        let window = adw::Window::builder()
            .application(app)
            .title(tr("Asistente"))
            .default_width(520)
            .default_height(620)
            .hide_on_close(true)
            .build();

        let header = adw::HeaderBar::new();
        let new_button = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text(tr("Nueva conversación"))
            .build();
        header.pack_start(&new_button);
        let explorer_button = gtk::Button::builder()
            .icon_name("view-list-bullet-symbolic")
            .tooltip_text(tr("Notas, tareas e historial"))
            .build();
        header.pack_end(&explorer_button);

        let view = ConversationView::new();

        let status_label = gtk::Label::new(None);
        status_label.add_css_class("dim-label");
        let status = gtk::Box::builder()
            .spacing(8)
            .halign(gtk::Align::Center)
            .margin_bottom(6)
            .visible(false)
            .build();
        status.append(&adw::Spinner::new());
        status.append(&status_label);

        let entry = gtk::Entry::builder()
            .placeholder_text(tr("Escribe o pulsa el micrófono…"))
            .hexpand(true)
            .build();
        let mic = gtk::Button::builder()
            .icon_name("audio-input-microphone-symbolic")
            .tooltip_text(tr("Grabar"))
            .build();
        let send = gtk::Button::builder()
            .icon_name("mail-send-symbolic")
            .tooltip_text(tr("Enviar"))
            .css_classes(["suggested-action"])
            .build();
        let input = gtk::Box::builder()
            .spacing(6)
            .margin_top(6)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        let attach = add_button(
            AddOptions::ImagesOnly,
            Rc::new(crate::config::SttConfig::default),
            Rc::new({
                let commands = commands.clone();
                move |attachment| {
                    let _ = commands.try_send(Command::AddPending(attachment));
                }
            }),
            Rc::new({
                let commands = commands.clone();
                move |message| {
                    let _ = commands.try_send(Command::AttachmentError(message));
                }
            }),
        );
        input.append(&attach);
        input.append(&entry);
        input.append(&mic);
        input.append(&send);
        // Imágenes que irán con el próximo mensaje.
        let pending = gtk::Box::builder().margin_start(12).margin_end(12).visible(false).build();

        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.append(view.widget());
        body.append(&status);
        body.append(&pending);
        body.append(&input);

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&body));
        window.set_content(Some(&toolbar));

        // Esc cierra (oculta) la ventana; Ctrl+V con una imagen en el portapapeles la adjunta.
        let keys = gtk::EventControllerKey::builder().propagation_phase(gtk::PropagationPhase::Capture).build();
        keys.connect_key_pressed(glib::clone!(
            #[weak]
            window,
            #[strong]
            commands,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, modifiers| {
                if key == gtk::gdk::Key::Escape {
                    window.close();
                    return glib::Propagation::Stop;
                }
                let paste = matches!(key, gtk::gdk::Key::v | gtk::gdk::Key::V) && modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK);
                let has_image = gtk::gdk::Display::default()
                    .is_some_and(|d| d.clipboard().formats().contains_type(gtk::gdk::Texture::static_type()));
                if paste && has_image {
                    let commands = commands.clone();
                    glib::spawn_future_local(async move {
                        let command = match crate::attachments::paste_image().await {
                            Ok(attachment) => Command::AddPending(attachment),
                            Err(err) => Command::AttachmentError(format!("{err:#}")),
                        };
                        let _ = commands.try_send(command);
                    });
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            }
        ));
        window.add_controller(keys);

        // Arrastrar imágenes a la ventana.
        let drop = gtk::DropTarget::new(gtk::gdk::FileList::static_type(), gtk::gdk::DragAction::COPY);
        drop.connect_drop(glib::clone!(
            #[strong]
            commands,
            move |_, value, _, _| {
                let Ok(files) = value.get::<gtk::gdk::FileList>() else { return false };
                for path in files.files().iter().filter_map(|f| f.path()) {
                    let command = match crate::attachments::import_image(&path) {
                        Ok(attachment) => Command::AddPending(attachment),
                        Err(err) => Command::AttachmentError(format!("{err:#}")),
                    };
                    let _ = commands.try_send(command);
                }
                true
            }
        ));
        window.add_controller(drop);

        let submit = {
            let commands = commands.clone();
            let entry = entry.clone();
            move || {
                let text = entry.text().trim().to_owned();
                if text.is_empty() {
                    // Intro sin texto mientras se graba: parar y enviar la grabación.
                    let _ = commands.try_send(Command::StopRecording);
                } else {
                    entry.set_text("");
                    let _ = commands.try_send(Command::SendText(text));
                }
            }
        };
        entry.connect_activate({
            let submit = submit.clone();
            move |_| submit()
        });
        send.connect_clicked(move |_| submit());
        for (button, command) in [
            (&mic, Command::TogglePopupRecording),
            (&new_button, Command::NewTextConversation),
            (&explorer_button, Command::ShowExplorer),
        ] {
            let commands = commands.clone();
            button.connect_clicked(move |_| {
                let _ = commands.try_send(command.clone());
            });
        }

        Self { window, view, status, status_label, entry, mic, send, pending, commands }
    }

    pub fn window(&self) -> &adw::Window {
        &self.window
    }

    pub fn view(&self) -> &ConversationView {
        &self.view
    }

    /// Muestra la ventana. Con el token de activación del atajo, GNOME le da el foco.
    pub fn present(&self, activation_token: Option<&str>) {
        if let Some(token) = activation_token {
            self.window.set_startup_id(token);
        }
        // Dar el foco a la entrada antes de mostrar evita que GTK enfoque (y
        // seleccione entero) el primer mensaje.
        GtkWindowExt::set_focus(&self.window, Some(&self.entry));
        self.window.present();
    }

    /// Muestra las imágenes pendientes de enviar.
    pub fn set_pending(&self, attachments: &[NewAttachment]) {
        while let Some(child) = self.pending.first_child() {
            self.pending.remove(&child);
        }
        self.pending.set_visible(!attachments.is_empty());
        if attachments.is_empty() {
            return;
        }
        let commands = self.commands.clone();
        let gallery = pending_widget(
            attachments,
            Rc::new(move |index| {
                let _ = commands.try_send(Command::RemovePending(index));
            }),
        );
        gallery.set_hexpand(true);
        self.pending.append(&gallery);
    }

    pub fn is_active(&self) -> bool {
        self.window.is_visible() && self.window.is_active()
    }

    /// Pregunta si se borra algo; la respuesta vuelve a la tool por `reply`.
    pub fn ask_confirmation(&self, question: &str, reply: async_channel::Sender<bool>) {
        let label = wrapped_label();
        label.set_text(question);
        label.set_selectable(false);

        let cancel = gtk::Button::with_label(&tr("Cancelar"));
        let delete = gtk::Button::builder().label(tr("Borrar")).css_classes(["destructive-action"]).build();
        let buttons = gtk::Box::builder().spacing(6).halign(gtk::Align::End).build();
        buttons.append(&cancel);
        buttons.append(&delete);

        let card = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(10).build();
        card.add_css_class("card");
        card.add_css_class("confirmation");
        card.append(&label);
        card.append(&buttons);
        self.view.append(&card);
        self.set_status(Some(&tr("Esperando confirmación…")));
        delete.grab_focus();

        let answer = Rc::new({
            let status = self.status_label.clone();
            move |confirmed: bool| {
                let _ = reply.try_send(confirmed);
                card.remove(&buttons);
                let result = gtk::Label::builder()
                    .label(if confirmed { tr("Borrado confirmado") } else { tr("Borrado cancelado") })
                    .halign(gtk::Align::End)
                    .css_classes(["dim-label", "caption"])
                    .build();
                card.append(&result);
                status.set_text(&tr("Pensando…"));
            }
        });
        delete.connect_clicked({
            let answer = answer.clone();
            move |_| answer(true)
        });
        cancel.connect_clicked(move |_| answer(false));
    }

    /// `Some(texto)` muestra el indicador de actividad y bloquea la entrada.
    pub fn set_status(&self, status: Option<&str>) {
        self.status.set_visible(status.is_some());
        self.status_label.set_text(status.unwrap_or_default());
        let busy = status.is_some();
        self.entry.set_sensitive(!busy);
        self.send.set_sensitive(!busy);
    }

    pub fn set_recording(&self, recording: bool) {
        if recording {
            self.mic.set_icon_name("media-playback-stop-symbolic");
            self.mic.set_tooltip_text(Some(&tr("Parar y enviar")));
            self.mic.add_css_class("destructive-action");
        } else {
            self.mic.set_icon_name("audio-input-microphone-symbolic");
            self.mic.set_tooltip_text(Some(&tr("Grabar")));
            self.mic.remove_css_class("destructive-action");
        }
    }
}
