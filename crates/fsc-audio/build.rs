// espeak-rs-sys builds a bundled espeak-ng with CMake. On Linux, CMake
// enables optional features for any libraries it finds installed: here
// pcaudiolib (audio output) and sonic (speed-up), both pulled in by the
// system espeak-ng package we rely on for phoneme data. The crate doesn't
// link them, so we do.
fn main() {
    println!("cargo:rustc-link-lib=dylib=pcaudio");
    println!("cargo:rustc-link-lib=dylib=sonic");
}
