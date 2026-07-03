//! 输出 OpenAPI 3.1 规范到 stdout。
//!
//! 官网文档页（Scalar）的数据源生成命令：
//!
//! ```bash
//! cargo run -p fluxdown_api --example gen_openapi > website/public/openapi.json
//! ```

fn main() {
    println!("{}", fluxdown_api::openapi::openapi_json());
}
