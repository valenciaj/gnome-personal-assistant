//! Widgets de adjuntos: galería de imágenes, lista de audios y menú para añadir.

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::attachments;
use crate::conversation_view::wrapped_label;
use crate::i18n::tr;
use crate::store::{Attachment, AttachmentKind, NewAttachment};

/// Galería de imágenes y lista de audios. Con `on_remove`, cada adjunto lleva un
/// botón para quitarlo.
pub fn attachments_widget(attachments: &[Attachment], on_remove: Option<Rc<dyn Fn(i64)>>) -> gtk::Box {
    let root = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).build();

    let images: Vec<&Attachment> = attachments.iter().filter(|a| a.kind == AttachmentKind::Image).collect();
    if !images.is_empty() {
        let gallery = gallery();
        for image in images {
            let remove = on_remove.clone().map(|f| {
                let id = image.id;
                Rc::new(move || f(id)) as Rc<dyn Fn()>
            });
            gallery.append(&thumbnail(&image.path, remove));
        }
        root.append(&gallery);
    }

    for audio in attachments.iter().filter(|a| a.kind == AttachmentKind::Audio) {
        let row = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).css_classes(["card", "attachment-audio"]).build();
        let controls = gtk::Box::builder().spacing(6).build();
        if std::path::Path::new(&audio.path).exists() {
            let media = gtk::MediaFile::for_filename(&audio.path);
            controls.append(&gtk::MediaControls::builder().media_stream(&media).hexpand(true).build());
        } else {
            let missing = gtk::Label::builder().label(tr("La grabación ya no existe")).hexpand(true).xalign(0.0).css_classes(["dim-label"]).build();
            controls.append(&missing);
        }
        if let Some(remove) = on_remove.clone() {
            let id = audio.id;
            let button = remove_button();
            button.connect_clicked(move |_| remove(id));
            controls.append(&button);
        }
        row.append(&controls);
        if let Some(transcript) = audio.transcript.as_deref().filter(|t| !t.is_empty()) {
            let label = wrapped_label();
            label.set_text(transcript);
            label.add_css_class("dim-label");
            row.append(&label);
        }
        root.append(&row);
    }
    root
}

/// Miniaturas de adjuntos aún sin guardar (popup), quitables por posición.
pub fn pending_widget(pending: &[NewAttachment], on_remove: Rc<dyn Fn(usize)>) -> gtk::FlowBox {
    let gallery = gallery();
    for (index, attachment) in pending.iter().enumerate() {
        let remove = on_remove.clone();
        gallery.append(&thumbnail(&attachment.path, Some(Rc::new(move || remove(index)))));
    }
    gallery
}

/// Miniatura recortada al tamaño exacto (si no, GTK usa el tamaño de la imagen).
pub fn thumbnail_picture(path: &str, width: i32, height: i32) -> gtk::Picture {
    let picture = gtk::Picture::builder().width_request(width).height_request(height).css_classes(["thumbnail"]).build();
    if let Some(texture) = thumbnail_texture(path, width, height) {
        picture.set_paintable(Some(&texture));
    }
    picture
}

fn thumbnail_texture(path: &str, width: i32, height: i32) -> Option<gtk::gdk::Texture> {
    use gtk::gdk_pixbuf::{InterpType, Pixbuf};
    let source = Pixbuf::from_file(path).ok()?;
    let scale = (f64::from(width) / f64::from(source.width())).max(f64::from(height) / f64::from(source.height()));
    let scaled = source.scale_simple(
        ((f64::from(source.width()) * scale).ceil() as i32).max(width),
        ((f64::from(source.height()) * scale).ceil() as i32).max(height),
        InterpType::Bilinear,
    )?;
    let cropped = scaled.new_subpixbuf((scaled.width() - width) / 2, (scaled.height() - height) / 2, width, height);
    let png = cropped.save_to_bufferv("png", &[]).ok()?;
    gtk::gdk::Texture::from_bytes(&glib::Bytes::from_owned(png)).ok()
}

fn gallery() -> gtk::FlowBox {
    gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .halign(gtk::Align::Start)
        .homogeneous(true)
        .min_children_per_line(2)
        .max_children_per_line(8)
        .column_spacing(6)
        .row_spacing(6)
        .build()
}

fn remove_button() -> gtk::Button {
    gtk::Button::builder()
        .icon_name("window-close-symbolic")
        .tooltip_text(tr("Quitar"))
        .css_classes(["circular", "osd"])
        .valign(gtk::Align::Start)
        .halign(gtk::Align::End)
        .margin_top(4)
        .margin_end(4)
        .build()
}

fn thumbnail(path: &str, on_remove: Option<Rc<dyn Fn()>>) -> gtk::Widget {
    let picture = thumbnail_picture(path, 120, 90);
    let open = gtk::Button::builder().child(&picture).css_classes(["flat", "thumbnail-button"]).tooltip_text(tr("Ver la imagen")).build();
    let path = path.to_owned();
    open.connect_clicked(move |button| show_image(button, &path));
    let overlay = gtk::Overlay::builder().child(&open).build();
    if let Some(remove) = on_remove {
        let button = remove_button();
        button.connect_clicked(move |_| remove());
        overlay.add_overlay(&button);
    }
    overlay.upcast()
}

/// Muestra una imagen a tamaño grande.
pub fn show_image(parent: &impl IsA<gtk::Widget>, path: &str) {
    let picture = gtk::Picture::builder().content_fit(gtk::ContentFit::Contain).can_shrink(true).build();
    picture.set_filename(Some(path));
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&picture));
    let dialog = adw::Dialog::builder().content_width(900).content_height(700).child(&toolbar).build();
    dialog.present(Some(parent));
}

/// Qué ofrece el menú de añadir adjuntos.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AddOptions {
    ImagesOnly,
    ImagesAndAudio,
}

/// Botón «Añadir» con las formas de obtener un adjunto. `on_added` recibe cada
/// adjunto ya copiado a la carpeta de datos; `on_error`, los fallos.
pub fn add_button(
    options: AddOptions,
    stt: Rc<dyn Fn() -> crate::config::SttConfig>,
    on_added: Rc<dyn Fn(NewAttachment)>,
    on_error: Rc<dyn Fn(String)>,
) -> gtk::MenuButton {
    let list = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).build();
    let popover = gtk::Popover::builder().child(&list).build();
    let button = gtk::MenuButton::builder()
        .icon_name("mail-attachment-symbolic")
        .tooltip_text(tr("Adjuntar"))
        .popover(&popover)
        .valign(gtk::Align::Center)
        .build();

    let mut entries: Vec<(&str, String, Source)> = vec![
        ("image-x-generic-symbolic", tr("Imagen desde archivo…"), Source::ImageFile),
        ("edit-paste-symbolic", tr("Pegar imagen"), Source::Paste),
        ("applets-screenshooter-symbolic", tr("Captura de pantalla"), Source::Screenshot),
        ("camera-photo-symbolic", tr("Foto con la cámara…"), Source::Camera),
    ];
    if options == AddOptions::ImagesAndAudio {
        entries.push(("audio-input-microphone-symbolic", tr("Grabar audio…"), Source::Record));
        entries.push(("audio-x-generic-symbolic", tr("Importar audio…"), Source::AudioFile));
    }
    for (icon, label, source) in entries {
        let content = adw::ButtonContent::builder().icon_name(icon).label(&label).halign(gtk::Align::Start).build();
        let item = gtk::Button::builder().child(&content).css_classes(["flat"]).build();
        let (popover, on_added, on_error, stt, anchor) = (popover.clone(), on_added.clone(), on_error.clone(), stt.clone(), button.clone());
        item.connect_clicked(move |_| {
            popover.popdown();
            let (on_added, on_error, stt, anchor) = (on_added.clone(), on_error.clone(), stt.clone(), anchor.clone());
            glib::spawn_future_local(async move {
                let window = anchor.root().and_downcast::<gtk::Window>();
                match obtain(source, window.as_ref(), &stt).await {
                    Ok(Some(attachment)) => on_added(attachment),
                    Ok(None) => {}
                    Err(err) => on_error(format!("{err:#}")),
                }
            });
        });
        list.append(&item);
    }
    button
}

#[derive(Clone, Copy)]
enum Source {
    ImageFile,
    Paste,
    Screenshot,
    Camera,
    Record,
    AudioFile,
}

async fn obtain(
    source: Source,
    window: Option<&gtk::Window>,
    stt: &Rc<dyn Fn() -> crate::config::SttConfig>,
) -> anyhow::Result<Option<NewAttachment>> {
    match source {
        Source::ImageFile => match pick_file(window, &tr("Elegir imagen"), "image/*").await {
            Some(path) => attachments::import_image(&path).map(Some),
            None => Ok(None),
        },
        Source::AudioFile => match pick_file(window, &tr("Elegir audio"), "audio/*").await {
            Some(path) => attachments::import_audio(&path).map(Some),
            None => Ok(None),
        },
        Source::Paste => attachments::paste_image().await.map(Some),
        Source::Screenshot => attachments::screenshot().await.map(Some),
        Source::Camera => crate::camera::take_photo(window).await,
        Source::Record => crate::record_dialog::record(window, stt()).await,
    }
}

async fn pick_file(window: Option<&gtk::Window>, title: &str, mime: &str) -> Option<std::path::PathBuf> {
    let filter = gtk::FileFilter::new();
    filter.add_mime_type(mime);
    let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    let dialog = gtk::FileDialog::builder().title(title).filters(&filters).default_filter(&filter).modal(true).build();
    dialog.open_future(window).await.ok()?.path()
}
