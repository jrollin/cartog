use crate::auth::middleware::{auth_middleware, extract_token};
use crate::auth::service::{AuthProvider, DefaultAuth};
use crate::auth::tokens::refresh_token;
use crate::config::Config;

use crate::Request;
use crate::Response;

pub fn login_handler(request: Request) -> Response {
    let config = Config::load();
    let auth = DefaultAuth::new(config);

    let email = "user@example.com";
    let password = "password";

    match auth.login(email, password) {
        Ok(token) => Response::ok(format!("{{\"token\": \"{token}\"}}")),
        Err(e) => Response::error(401, &format!("{e}")),
    }
}

pub fn logout_handler(request: Request) -> Response {
    let config = Config::load();
    let auth = DefaultAuth::new(config);

    match extract_token(&request) {
        Some(token) => match auth.logout(&token) {
            Ok(_) => Response::ok("Logged out".to_string()),
            Err(e) => Response::error(500, &format!("{e}")),
        },
        None => Response::error(401, "Missing token"),
    }
}

pub fn refresh_handler(request: Request) -> Response {
    let config = Config::load();

    match extract_token(&request) {
        Some(token) => match refresh_token(&token, &config) {
            Ok(new_token) => Response::ok(format!("{{\"token\": \"{}\"}}", new_token.value)),
            Err(e) => Response::error(401, &e.message),
        },
        None => Response::error(401, "Missing token"),
    }
}
