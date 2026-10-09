//! Compila las traducciones (po/*.po) a catálogos .mo que se incrustan en el binario.

use std::path::PathBuf;
use std::process::Command;

const LANGUAGES: &[&str] = &["en"];

fn main() {
    println!("cargo:rerun-if-changed=po");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    for lang in LANGUAGES {
        let po = format!("po/{lang}.po");
        let mo = out.join(format!("{lang}.mo"));
        let compiled = Command::new("msgfmt").arg("-o").arg(&mo).arg(&po).status().is_ok_and(|s| s.success());
        if !compiled {
            // Sin msgfmt (paquete gettext) la app funciona igual, solo en español.
            println!("cargo:warning=No se pudo compilar {po}; instala gettext para tener traducciones");
            std::fs::write(&mo, []).expect("escribir .mo vacío");
        }
    }
}
