fn main() {
    println!(
        "cargo:rustc-link-search={}",
        std::env::var("OUT_DIR").unwrap()
    );
    println!("cargo:rerun-if-changed=memory.x");
    std::fs::copy(
        "memory.x",
        format!("{}/memory.x", std::env::var("OUT_DIR").unwrap()),
    )
    .unwrap();
}
