//! Runtime de tokio compartido para HTTP (reqwest) y D-Bus (zbus vía ksni/ashpd).

use std::sync::OnceLock;

use tokio::runtime::Runtime;

pub fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("assistant-io")
            .enable_all()
            .build()
            .expect("no se pudo crear el runtime de tokio")
    })
}
