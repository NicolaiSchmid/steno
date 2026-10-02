//! Compiles the vendored SpeexDSP echo canceller and preprocessor
//! (`vendor/speexdsp/`, BSD licence, see COPYING there) into the binary.
//! No system library, no pkg-config, no bindgen: the FFI surface is
//! declared by hand in `src/speex.rs`.
fn main() {
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
    build
        .include("vendor/speexdsp")
        .define("FLOATING_POINT", None)
        .define("USE_KISS_FFT", None)
        .define("EXPORT", Some(""))
        .warnings(false)
        .opt_level(2)
        .compile("speexdsp_spike");
}
