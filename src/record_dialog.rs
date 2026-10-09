//! Grabar un audio para adjuntarlo desde el explorador. Al guardar se transcribe
//! para que la transcripción se pueda buscar.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use anyhow::Result;
use gtk::glib;

use crate::attachments;
use crate::audio::Recorder;
use crate::config::{self, SttConfig};
use crate::i18n::tr;
use crate::runtime::runtime;
use crate::store::NewAttachment;

/// Devuelve la grabación (con su transcripción, si se pudo hacer) o `None` si se cancela.
pub async fn record(parent: Option<&gtk::Window>, stt: SttConfig) -> Result<Option<NewAttachment>> {
    let time = gtk::Label::builder().label("0:00").css_classes(["numeric", "title-1"]).build();
    let level = gtk::LevelBar::builder().min_value(0.0).max_value(1.0).width_request(240).build();
    let toggle = gtk::Button::builder()
        .icon_name("media-record-symbolic")
        .tooltip_text(tr("Grabar"))
        .css_classes(["circular", "destructive-action"])
        .width_request(64)
        .height_request(64)
        .halign(gtk::Align::Center)
        .build();
    let status = gtk::Label::builder().label(tr("Pulsa para empezar a grabar")).css_classes(["dim-label"]).build();
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(18)
        .margin_bottom(18)
        .margin_start(24)
        .margin_end(24)
        .build();
    for widget in [time.upcast_ref::<gtk::Widget>(), level.upcast_ref(), toggle.upcast_ref(), status.upcast_ref()] {
        content.append(widget);
    }
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&content));
    let dialog = adw::Dialog::builder().title(tr("Grabar audio")).child(&toolbar).build();

    let recorder: Rc<RefCell<Option<Recorder>>> = Rc::default();
    let (done_tx, done_rx) = async_channel::bounded::<Option<std::path::PathBuf>>(1);

    toggle.connect_clicked(glib::clone!(
        #[strong]
        recorder,
        #[strong]
        done_tx,
        #[weak]
        time,
        #[weak]
        level,
        #[weak]
        status,
        move |button| {
            if let Some(active) = recorder.borrow_mut().take() {
                button.set_sensitive(false);
                status.set_text(&tr("Transcribiendo…"));
                let done_tx = done_tx.clone();
                glib::spawn_future_local(async move {
                    let _ = done_tx.send(active.stop().await.ok()).await;
                });
                return;
            }
            let path = config::audio_dir().join(format!("nota-{}.ogg", chrono::Local::now().format("%Y%m%d-%H%M%S")));
            let level_bar = level.clone();
            match Recorder::start(&path, "", move |db| level_bar.set_value(((db + 60.0) / 60.0).clamp(0.0, 1.0)), |_| {}) {
                Ok(active) => {
                    *recorder.borrow_mut() = Some(active);
                    button.set_icon_name("media-playback-stop-symbolic");
                    button.set_tooltip_text(Some(&tr("Parar")));
                    status.set_text(&tr("Grabando…"));
                    let started = Instant::now();
                    let weak = recorder.clone();
                    glib::timeout_add_local(Duration::from_millis(250), glib::clone!(
                        #[weak]
                        time,
                        #[upgrade_or]
                        glib::ControlFlow::Break,
                        move || {
                            if weak.borrow().is_none() {
                                return glib::ControlFlow::Break;
                            }
                            let seconds = started.elapsed().as_secs();
                            time.set_text(&format!("{}:{:02}", seconds / 60, seconds % 60));
                            glib::ControlFlow::Continue
                        }
                    ));
                }
                Err(err) => status.set_text(&format!("{err:#}")),
            }
        }
    ));
    dialog.connect_closed(glib::clone!(
        #[strong]
        recorder,
        move |_| {
            if let Some(active) = recorder.borrow_mut().take() {
                active.cancel();
            }
            let _ = done_tx.try_send(None);
        }
    ));
    dialog.present(parent);

    let Some(path) = done_rx.recv().await.ok().flatten() else { return Ok(None) };
    let transcript = {
        let path = path.clone();
        runtime().spawn(async move { crate::stt::transcribe(&stt, &path).await }).await.ok().and_then(Result::ok)
    };
    dialog.close();
    Ok(Some(attachments::audio_attachment(&path, transcript.filter(|t| !t.is_empty()))))
}
