// Make sure the embed folder exists so `cargo build` works before the
// frontend is built. Run `pnpm -C app build` for the real UI.
use std::{fs, path::Path};

fn main() {
    let dist = Path::new("app/dist");
    let index = dist.join("index.html");
    if !index.exists() {
        fs::create_dir_all(dist).expect("create app/dist");
        fs::write(
            &index,
            "<!doctype html><title>Aside</title><p>Frontend not built. Run <code>pnpm -C app install && pnpm -C app build</code>, then rebuild aside.</p>",
        )
        .expect("write placeholder index.html");
    }
    println!("cargo:rerun-if-changed=app/dist");
}
