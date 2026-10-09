//! Archivos adjuntos: guardar imágenes y audios en la carpeta de datos, prepararlos
//! para el modelo y obtenerlos del portapapeles o de una captura de pantalla.

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use gtk::gdk_pixbuf::{InterpType, Pixbuf};
use gtk::prelude::*;
use gtk::{gdk, glib};

use crate::config;
use crate::i18n::{tr, trf};
use crate::runtime::runtime;
use crate::store::{AttachmentKind, NewAttachment};

/// Lado mayor con el que se envían las imágenes al modelo (más no mejora la visión).
const MODEL_MAX_SIDE: i32 = 1568;

fn unique_name(prefix: &str, extension: &str) -> String {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S-%3f");
    format!("{prefix}-{stamp}.{extension}")
}

fn image_mime(extension: &str) -> Option<&'static str> {
    Some(match extension.to_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "svg" => "image/svg+xml",
        _ => return None,
    })
}

fn audio_mime(extension: &str) -> Option<&'static str> {
    Some(match extension.to_lowercase().as_str() {
        "ogg" | "oga" | "opus" => "audio/ogg",
        "mp3" => "audio/mpeg",
        "m4a" | "aac" => "audio/mp4",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "webm" => "audio/webm",
        _ => return None,
    })
}

pub fn audio_attachment(path: &Path, transcript: Option<String>) -> NewAttachment {
    NewAttachment {
        kind: AttachmentKind::Audio,
        path: path.to_string_lossy().into_owned(),
        mime: "audio/ogg".into(),
        transcript,
    }
}

/// Copia una imagen a la carpeta de datos.
pub fn import_image(source: &Path) -> Result<NewAttachment> {
    let extension = source.extension().and_then(|e| e.to_str()).unwrap_or_default().to_lowercase();
    let mime = image_mime(&extension).ok_or_else(|| anyhow!(trf("No es una imagen admitida: {path}", &[("path", &source.display())])))?;
    // Comprobar que realmente se puede abrir.
    Pixbuf::from_file(source).with_context(|| trf("No se pudo abrir la imagen {path}", &[("path", &source.display())]))?;
    let target = config::images_dir().join(unique_name("img", &extension));
    copy_into(source, &target)?;
    Ok(NewAttachment { kind: AttachmentKind::Image, path: target.to_string_lossy().into_owned(), mime: mime.into(), transcript: None })
}

/// Copia un archivo de audio a la carpeta de datos.
pub fn import_audio(source: &Path) -> Result<NewAttachment> {
    let extension = source.extension().and_then(|e| e.to_str()).unwrap_or_default().to_lowercase();
    let mime = audio_mime(&extension).ok_or_else(|| anyhow!(trf("No es un audio admitido: {path}", &[("path", &source.display())])))?;
    let target = config::audio_dir().join(unique_name("import", &extension));
    copy_into(source, &target)?;
    Ok(NewAttachment { kind: AttachmentKind::Audio, path: target.to_string_lossy().into_owned(), mime: mime.into(), transcript: None })
}

fn copy_into(source: &Path, target: &Path) -> Result<()> {
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::copy(source, target).with_context(|| trf("No se pudo copiar {path}", &[("path", &source.display())]))?;
    Ok(())
}

/// Guarda una imagen en memoria (portapapeles, cámara) como PNG.
pub fn save_texture(texture: &gdk::Texture) -> Result<NewAttachment> {
    let target = config::images_dir().join(unique_name("img", "png"));
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir)?;
    }
    texture.save_to_png(&target)?;
    Ok(NewAttachment { kind: AttachmentKind::Image, path: target.to_string_lossy().into_owned(), mime: "image/png".into(), transcript: None })
}

/// Imagen del portapapeles, si la hay.
pub async fn paste_image() -> Result<NewAttachment> {
    let display = gdk::Display::default().ok_or_else(|| anyhow!("sin display"))?;
    let texture = display
        .clipboard()
        .read_texture_future()
        .await
        .ok()
        .flatten()
        .ok_or_else(|| anyhow!(tr("No hay ninguna imagen en el portapapeles")))?;
    save_texture(&texture)
}

/// Captura de pantalla con el diálogo de GNOME (permite elegir una zona).
pub async fn screenshot() -> Result<NewAttachment> {
    let uri = runtime()
        .spawn(async {
            let response = ashpd::desktop::screenshot::Screenshot::request().interactive(true).modal(true).send().await?.response()?;
            anyhow::Ok(response.uri().to_string())
        })
        .await??;
    let path = glib::filename_from_uri(&uri).map(|(path, _)| path).map_err(|_| anyhow!("URI no válida: {uri}"))?;
    import_image(&path)
}

/// Imagen lista para el modelo: reducida y en JPEG, codificada en base64.
pub struct ModelImage {
    pub mime: String,
    pub base64: String,
}

pub fn model_image(path: &str) -> Result<ModelImage> {
    let pixbuf = Pixbuf::from_file(path).with_context(|| trf("No se pudo abrir la imagen {path}", &[("path", &path)]))?;
    let (width, height) = (pixbuf.width(), pixbuf.height());
    let scale = (f64::from(MODEL_MAX_SIDE) / f64::from(width.max(height))).min(1.0);
    let (w, h) = (((f64::from(width) * scale) as i32).max(1), ((f64::from(height) * scale) as i32).max(1));
    // JPEG no tiene transparencia: se compone sobre fondo blanco.
    let flat = if pixbuf.has_alpha() {
        pixbuf.composite_color_simple(w, h, InterpType::Bilinear, 255, 64, 0xffffff, 0xffffff)
    } else {
        pixbuf.scale_simple(w, h, InterpType::Bilinear)
    }
    .ok_or_else(|| anyhow!("no se pudo escalar la imagen"))?;
    let bytes = flat.save_to_bufferv("jpeg", &[("quality", "85")])?;
    Ok(ModelImage { mime: "image/jpeg".into(), base64: glib::base64_encode(&bytes).to_string() })
}
