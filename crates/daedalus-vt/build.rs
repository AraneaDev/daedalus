fn main() {
    #[cfg(feature = "ghostty-vt")]
    native::build();
}

#[cfg(feature = "ghostty-vt")]
mod native {
    use std::path::PathBuf;

    pub fn build() {
        println!("cargo:rerun-if-env-changed=GHOSTTY_VT_DIR");
        println!("cargo:rerun-if-changed=src/ghostty/native.c");
        let mut build = cc::Build::new();
        build.file("src/ghostty/native.c");
        if let Some(dir) = std::env::var_os("GHOSTTY_VT_DIR") {
            let root = PathBuf::from(dir);
            println!(
                "cargo:rerun-if-changed={}",
                root.join("include/ghostty").display()
            );
            build
                .include(root.join("include"))
                .compile("daedalus_ghostty");
            println!(
                "cargo:rustc-link-search=native={}",
                root.join("lib").display()
            );
            println!("cargo:rustc-link-lib=ghostty-vt");
        } else {
            let package = pkg_config::Config::new().cargo_metadata(false)
                .probe("libghostty-vt").expect("install the pinned libghostty-vt or set GHOSTTY_VT_DIR to its installation prefix");
            build
                .includes(&package.include_paths)
                .compile("daedalus_ghostty");
            pkg_config::probe_library("libghostty-vt").unwrap();
        }
    }
}
