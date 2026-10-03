//! Compiles the vendored `SpeexDSP` echo canceller and preprocessor
//! (`vendor/speexdsp/`, BSD licence, `COPYING` there; provenance in
//! `vendor/speexdsp/PROVENANCE`) into the crate. No system library, no
//! pkg-config, no bindgen: the FFI surface is declared by hand in
//! `src/aec/speex.rs`. Seven files, floating point, KISS FFT: in effect
//! the Swift package's `CSpeex` target. `DISABLE_WARNINGS` silences every
//! `speex_warning` (it gates them all in `os_support.h`), among them "The
//! VAD has been replaced by a hack" on every canceller init, which the
//! Swift build prints to stderr; it changes no arithmetic.
fn main() {
    println!("cargo:rerun-if-env-changed=STENO_AUDIO_SKIP_SPEEX");
    // `cargo check --target aarch64-apple-darwin` from a Linux box has no
    // Apple SDK to compile C against; the Rust side only declares the FFI
    // and never links in a check, so the C build may be skipped for it.
    if std::env::var_os("STENO_AUDIO_SKIP_SPEEX").is_some() {
        return;
    }
    let files = [
        "mdf.c",
        "preprocess.c",
        "filterbank.c",
        "fftwrap.c",
        "kiss_fft.c",
        "kiss_fftr.c",
        "smallft.c",
    ];
    let mut build = cc::Build::new();
    for file in files {
        let path = format!("vendor/speexdsp/{file}");
        println!("cargo:rerun-if-changed={path}");
        build.file(path);
    }
    println!("cargo:rerun-if-changed=vendor/speexdsp/speex");
    build
        .include("vendor/speexdsp")
        .define("FLOATING_POINT", None)
        .define("USE_KISS_FFT", None)
        .define("EXPORT", Some(""))
        .define("DISABLE_WARNINGS", None)
        .warnings(false)
        .opt_level(2)
        .compile("speexdsp");
}
