#[tokio::main]
async fn main() {
    if std::env::args().nth(1).as_deref() == Some("--build-info") {
        println!(
            "{}",
            tokemula_image_connector::distribution::build_info("tokemula-image")
        );
        return;
    }
    std::process::exit(tokemula_image_connector::management::run(std::env::args_os()).await);
}
