// espeak-rs-sys builds a bundled espeak-ng with CMake, which turns on
// optional features for libraries it finds installed: pcaudiolib (audio
// output) and sonic (speed-up). On Linux the system espeak-ng package (whose
// phoneme data we use) pulls both in, and the crate doesn't link them, so
// we do. Elsewhere they aren't installed: espeak-ng then leaves pcaudio out
// and builds sonic into itself.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-link-lib=dylib=pcaudio");
        println!("cargo:rustc-link-lib=dylib=sonic");
    }
}
