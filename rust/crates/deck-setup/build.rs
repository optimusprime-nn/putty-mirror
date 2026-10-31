fn main() {
    println!("cargo:rerun-if-env-changed=DECK_EXE");
    println!("cargo:rerun-if-env-changed=DECK_TOOLS");
    if let Ok(dir) = std::env::var("DECK_TOOLS") {
        for tool in ["plink", "pscp", "psftp", "puttygen", "pageant"] {
            println!("cargo:rerun-if-changed={dir}/{tool}.exe");
        }
    }
    if let Ok(exe) = std::env::var("DECK_EXE") {
        println!("cargo:rerun-if-changed={exe}");
    }
    println!("cargo:rerun-if-changed=../puttydeck/assets");
    embed_resource::compile("../puttydeck/assets/deck.rc", embed_resource::NONE).manifest_required().unwrap();
}
