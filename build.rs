// Make sure the embed folder exists so `cargo build` works before the
// frontend is built. Run `pnpm -C web build` for the real UI.
use std::{fs, path::Path};

fn main() {
    let dist = Path::new("web/dist");
    let index = dist.join("index.html");
    if !index.exists() {
        fs::create_dir_all(dist).expect("create web/dist");
        fs::write(
            &index,
            "<!doctype html><title>Aside</title><p>Frontend not built. Run <code>pnpm -C web install && pnpm -C web build</code>, then rebuild aside.</p>",
        )
        .expect("write placeholder index.html");
    }
    println!("cargo:rerun-if-changed=web/dist");
}
