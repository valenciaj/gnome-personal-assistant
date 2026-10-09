//! Grabación del micrófono con GStreamer (PipeWire → Ogg/Opus).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use gst::prelude::*;

pub struct Recorder {
    pipeline: gst::Pipeline,
    path: PathBuf,
    started: Instant,
    eos: async_channel::Receiver<Result<(), String>>,
    _bus_watch: gst::bus::BusWatchGuard,
}

impl Recorder {
    /// Empieza a grabar en `path`. `on_level` recibe el nivel RMS en dBFS cada 50 ms;
    /// `on_error` se llama si el pipeline falla durante la grabación.
    pub fn start(
        path: &Path,
        on_level: impl Fn(f64) + 'static,
        on_error: impl Fn(String) + 'static,
    ) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let description = format!(
            "pipewiresrc ! audioconvert ! audioresample ! audio/x-raw,channels=1,rate=48000 \
             ! level interval=50000000 post-messages=true \
             ! opusenc bitrate=32000 ! oggmux ! filesink location=\"{}\"",
            path.display()
        );
        let pipeline = gst::parse::launch(&description)
            .context("No se pudo crear el pipeline de audio")?
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
                    let text = format!("Error de audio: {}", err.error());
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
            .context("No se pudo abrir el micrófono")?;

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
            Ok(Err(_)) => Err(anyhow!("El pipeline de audio se cerró inesperadamente")),
            Err(_) => Err(anyhow!("La grabación no terminó a tiempo")),
        }
    }

    /// Descarta la grabación.
    pub fn cancel(self) {
        let _ = self.pipeline.set_state(gst::State::Null);
        let _ = std::fs::remove_file(&self.path);
    }
}
