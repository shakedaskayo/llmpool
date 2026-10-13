//! Stamps the build, so a client can tell that the daemon it found was built
//! from other source than itself and must be restarted.
fn main() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    println!("cargo:rustc-env=LLMPOOL_BUILD={stamp}");
    for dir in [
        "src",
        "../llmpool-core/src",
        "../llmpool-adapter-claude/src",
    ] {
        println!("cargo:rerun-if-changed={dir}");
    }
}
