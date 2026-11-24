fn main() {
    println!("cargo:rerun-if-changed=spark.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    embed_resource::compile("spark.rc", embed_resource::NONE)
        .manifest_optional()
        .ok();
}
