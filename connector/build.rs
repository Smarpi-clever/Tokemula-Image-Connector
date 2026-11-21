fn main() {
    println!(
        "cargo:rustc-env=TOKEMULA_BUILD_TARGET={}",
        std::env::var("TARGET").expect("Cargo target")
    );
}
