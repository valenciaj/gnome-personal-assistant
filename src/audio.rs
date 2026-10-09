//! Grabación del micrófono con GStreamer (PipeWire → Ogg/Opus).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use gst::prelude::*;
use crate::i18n::{tr, trf};

pub struct Recorder {
    pipeline: gst::Pipeline,
    path: PathBuf,
    started: Instant,
    eos: async_channel::Receiver<Result<(), String>>,
    _bus_watch: gst::bus::BusWatchGuard,
}

impl Recorder {
    /// Empieza a grabar en `path` desde `device` (nodo de PipeWire; vacío = el
    /// predeterminado). `on_level` recibe el nivel RMS en dBFS cada 50 ms;
    /// `on_error` se llama si el pipeline falla durante la grabación.
    pub fn start(
        path: &Path,
        device: &str,
        on_level: impl Fn(f64) + 'static,
        on_error: impl Fn(String) + 'static,
    ) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let source = if device.is_empty() {
            "pipewiresrc".to_owned()
        } else {
            format!("pipewiresrc target-object=\"{}\"", device.replace('"', ""))
        };
        let description = format!(
            "{source} ! audioconvert ! audioresample ! audio/x-raw,channels=1,rate=48000 \
             ! level interval=50000000 post-messages=true \
             ! opusenc bitrate=32000 ! oggmux ! filesink location=\"{}\"",
            path.display()
        );
        let pipeline = gst::parse::launch(&description)
            .with_context(|| tr("No se pudo crear el pipeline de audio"))?
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow!("El pipeline de audio no es un gst::Pipeline"))?;

        let (eos_tx, eos) = async_channel::bounded(1);
        let bus = pipeline.bus().context("El pipeline no tiene bus")?;
        let bus_watch = bus.add_watch_local(move |_, message| {
            use gst::MessageView;
            match message.view() {
                MessageView::Element(element) => {
                    if let Some(rms) = element
                        .structure()
                        .filter(|s| s.name() == "level")
                        .and_then(|s| s.get::<gst::glib::ValueArray>("rms").ok())
                    {
                        let peak = rms
                            .iter()
                            .filter_map(|v| v.get::<f64>().ok())
                            .fold(f64::NEG_INFINITY, f64::max);
                        on_level(peak);
                    }
                }
                MessageView::Eos(_) => {
                    let _ = eos_tx.try_send(Ok(()));
                }
                MessageView::Error(err) => {
                    let text = trf("Error de audio: {error}", &[("error", &err.error())]);
                    log::error!("{text} ({:?})", err.debug());
                    let _ = eos_tx.try_send(Err(text.clone()));
                    on_error(text);
                }
                _ => {}
            }
            gst::glib::ControlFlow::Continue
        })?;

        pipeline
            .set_state(gst::State::Playing)
            .with_context(|| tr("No se pudo abrir el micrófono"))?;

        Ok(Self {
            pipeline,
            path: path.to_owned(),
            started: Instant::now(),
            eos,
            _bus_watch: bus_watch,
        })
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Para la grabación cerrando bien el archivo (EOS) y devuelve su ruta.
    pub async fn stop(self) -> Result<PathBuf> {
        self.pipeline.send_event(gst::event::Eos::new());
        let result = gst::glib::future_with_timeout(Duration::from_secs(5), self.eos.recv()).await;
        let _ = self.pipeline.set_state(gst::State::Null);
        match result {
            Ok(Ok(Ok(()))) => Ok(self.path.clone()),
            Ok(Ok(Err(err))) => Err(anyhow!(err)),
            Ok(Err(_)) => Err(anyhow!(tr("El pipeline de audio se cerró inesperadamente"))),
            Err(_) => Err(anyhow!(tr("La grabación no terminó a tiempo"))),
        }
    }

    /// Descarta la grabación.
    pub fn cancel(self) {
        let _ = self.pipeline.set_state(gst::State::Null);
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Micrófonos disponibles: (nombre visible, nodo de PipeWire).
pub fn input_devices() -> Vec<(String, String)> {
    let monitor = gst::DeviceMonitor::new();
    monitor.add_filter(Some("Audio/Source"), None);
    if monitor.start().is_err() {
        return Vec::new();
    }
    let devices = monitor
        .devices()
        .into_iter()
        .filter_map(|device| {
            let node = device.properties()?.get::<String>("node.name").ok()?;
            // Las «fuentes» de monitor son la salida de los altavoces, no micrófonos.
            (!node.ends_with(".monitor")).then(|| (device.display_name().to_string(), node))
        })
        .collect();
    monitor.stop();
    devices
}

#[derive(Debug, Clone, Copy)]
pub enum Sound {
    Start,
    Stop,
    Error,
    Alarm,
}

/// Reproduce un sonido corto del tema de sonidos del sistema.
pub fn play(sound: Sound) {
    use gtk::prelude::*;
    thread_local! {
        static PLAYING: std::cell::RefCell<Option<gtk::MediaFile>> = const { std::cell::RefCell::new(None) };
    }
    let name = match sound {
        Sound::Start => "device-added",
        Sound::Stop => "device-removed",
        Sound::Error => "dialog-warning",
        Sound::Alarm => "alarm-clock-elapsed",
    };
    let Some(path) = ["/usr/share/sounds/Yaru/stereo", "/usr/share/sounds/freedesktop/stereo"]
        .iter()
        .map(|dir| Path::new(dir).join(format!("{name}.oga")))
        .find(|p| p.exists())
    else {
        return;
    };
    let media = gtk::MediaFile::for_filename(&path);
    media.play();
    // Se guarda para que no se destruya (y calle) antes de terminar.
    PLAYING.with(|playing| *playing.borrow_mut() = Some(media));
}
