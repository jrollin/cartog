mod auth;
mod config;
mod error;
mod models;
mod routes;

use config::Config;
use routes::auth::{login_handler, logout_handler, refresh_handler};

fn main() {
    let config = Config::load();
    run_server(config);
}

fn run_server(config: Config) {
    println!("Starting server on port {}", config.port);
    let router = build_router();
    router.serve(config.port);
}

fn build_router() -> Router {
    let mut router = Router::new();
    router.add("/login", login_handler);
    router.add("/logout", logout_handler);
    router.add("/refresh", refresh_handler);
    router
}

struct Router {
    routes: Vec<(String, fn(Request) -> Response)>,
}

impl Router {
    fn new() -> Self {
        Self { routes: vec![] }
    }

    fn add(&mut self, path: &str, handler: fn(Request) -> Response) {
        self.routes.push((path.to_string(), handler));
    }

    fn serve(&self, port: u16) {
        println!("Listening on port {port}");
    }
}

pub struct Request {
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

pub struct Response {
    pub status: u16,
    pub body: String,
}

impl Response {
    pub fn ok(body: String) -> Self {
        Self { status: 200, body }
    }

    pub fn error(status: u16, message: &str) -> Self {
        Self {
            status,
            body: message.to_string(),
        }
    }
}
