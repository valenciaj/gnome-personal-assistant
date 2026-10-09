//! Foto con la cámara web: vista previa en directo y captura de un fotograma.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use anyhow::{Context, Result, anyhow};
use gst::prelude::*;
use gtk::{gdk, glib};

use crate::attachments;
use crate::i18n::tr;
use crate::store::NewAttachment;

/// Abre la cámara; devuelve la foto, o `None` si se cancela.
pub async fn take_photo(parent: Option<&gtk::Window>) -> Result<Option<NewAttachment>> {
    let pipeline = gst::parse::launch(
        "v4l2src ! videoconvert ! videoscale ! video/x-raw,format=RGBA,width=[1,1280] \
         ! appsink name=sink max-buffers=1 drop=true sync=false",
    )
    .context(tr("No se pudo abrir la cámara"))?
    .downcast::<gst::Pipeline>()
    .map_err(|_| anyhow!("pipeline"))?;
    let sink = pipeline
        .by_name("sink")
        .and_then(|e| e.downcast::<gst_app::AppSink>().ok())
        .ok_or_else(|| anyhow!("appsink"))?;

    // Los fotogramas llegan en el hilo de GStreamer y se pintan en el de GTK.
    let (frames, frames_rx) = async_channel::bounded::<(glib::Bytes, i32, i32)>(1);
    sink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let size = sample.caps().and_then(|caps| {
                    let s = caps.structure(0)?;
                    Some((s.get::<i32>("width").ok()?, s.get::<i32>("height").ok()?))
                });
                if let (Some(buffer), Some((width, height))) = (sample.buffer(), size) {
                    if let Ok(map) = buffer.map_readable() {
                        let _ = frames.try_send((glib::Bytes::from(map.as_slice()), width, height));
                    }
                }
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
    pipeline.set_state(gst::State::Playing).context(tr("No se pudo abrir la cámara"))?;

    let picture = gtk::Picture::builder().content_fit(gtk::ContentFit::Contain).width_request(480).height_request(360).build();
    let shoot = gtk::Button::builder().label(tr("Hacer foto")).css_classes(["suggested-action", "pill"]).halign(gtk::Align::Center).build();
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    content.append(&picture);
    content.append(&shoot);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&content));
    let dialog = adw::Dialog::builder().title(tr("Cámara")).child(&toolbar).build();

    let last: Rc<RefCell<Option<gdk::Texture>>> = Rc::default();
    glib::spawn_future_local({
        let (picture, last) = (picture.clone(), last.clone());
        async move {
            while let Ok((bytes, width, height)) = frames_rx.recv().await {
                let texture = gdk::MemoryTexture::new(width, height, gdk::MemoryFormat::R8g8b8a8, &bytes, (width * 4) as usize);
                picture.set_paintable(Some(&texture));
                *last.borrow_mut() = Some(texture.upcast());
            }
        }
    });

    let (result_tx, result_rx) = async_channel::bounded::<Option<gdk::Texture>>(1);
    shoot.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        #[strong]
        last,
        #[strong]
        result_tx,
        move |_| {
            let _ = result_tx.try_send(last.borrow().clone());
            dialog.close();
        }
    ));
    dialog.connect_closed(move |_| {
        let _ = result_tx.try_send(None);
    });
    dialog.present(parent);

    let texture = result_rx.recv().await.ok().flatten();
    let _ = pipeline.set_state(gst::State::Null);
    texture.map(|t| attachments::save_texture(&t)).transpose()
}
