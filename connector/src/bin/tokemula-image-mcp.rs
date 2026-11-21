use rmcp::{ServiceExt, service::ServerInitializeError};
use tokemula_image_connector::mcp::ImageMcp;

#[tokio::main]
async fn main() {
    if std::env::args().nth(1).as_deref() == Some("--build-info") {
        println!(
            "{}",
            tokemula_image_connector::distribution::build_info("tokemula-image-mcp")
        );
        return;
    }
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!(
            "tokemula-image-mcp {}",
            tokemula_image_connector::distribution::VERSION
        );
        return;
    }
    tokemula_image_connector::distribution::start_automatic_check();
    let mcp = match ImageMcp::for_current_user() {
        Ok(mcp) => mcp,
        Err(_) => {
            eprintln!("无法初始化图片连接配置。");
            std::process::exit(1);
        }
    };
    match mcp.serve(rmcp::transport::stdio()).await {
        Ok(server) => {
            if server.waiting().await.is_err() {
                eprintln!("MCP 连接已结束。");
                std::process::exit(1);
            }
        }
        Err(ServerInitializeError::ConnectionClosed(_)) => {}
        Err(_) => {
            eprintln!("MCP 协议初始化失败。");
            std::process::exit(1);
        }
    }
}
