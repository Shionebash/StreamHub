//! Enlaza `libmpv2-sys` contra el import library incluido en `libmpv/`.
//!
//! `libmpv2-sys` emite `cargo:rustc-link-lib=mpv`; aquí solo añadimos la
//! carpeta donde vive `mpv.lib` para que el linker lo encuentre. La DLL real
//! (`libmpv-2.dll`) se copia junto al ejecutable.

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    println!("cargo:rustc-link-search=native={manifest}/libmpv");
    println!("cargo:rerun-if-changed=libmpv/mpv.lib");
}
