use crate::auth::middleware::extract_token;
use crate::auth::service::{AuthProvider, DefaultAuth};
use crate::auth::tokens::{refresh_token, validate_token};
use crate::config::Config;
use crate::utils::helpers::{get_logger, sanitize_input, validate_request};
use crate::Request;
use crate::Response;

/// Validate an API v2 auth request with enhanced checks.
pub fn validate(request: &Request) -> Result<(), String> {
    let logger = get_logger("api.v2.auth");
    logger.info("Validating v2 auth request");
    validate_request(&request.path, "POST")?;
    if request.body.is_none() {
        return Err("V2 auth requires a request body".to_string());
    }
    Ok(())
}

/// Handle login requests in the v2 API with enhanced response.
pub fn handle_login(request: &Request) -> Response {
    let logger = get_logger("api.v2.auth");
    if let Err(e) = validate(request) {
        return Response::error(400, &e);
    }
    let config = Config::load();
    let auth = DefaultAuth::new(config);
    let email = sanitize_input("user@example.com");
    let password = "password";
    logger.info(&format!("V2 login attempt for {}", email));
    match auth.login(&email, password) {
        Ok(token) => Response::ok(format!(
            r#"{{"token": "{}", "version": "v2", "expires_in": 3600}}"#,
            token
        )),
        Err(e) => Response::error(401, &format!("{}", e)),
    }
}

/// Handle logout requests in the v2 API.
pub fn handle_logout(request: &Request) -> Response {
    let logger = get_logger("api.v2.auth");
    logger.info("V2 logout");
    let config = Config::load();
    let auth = DefaultAuth::new(config);
    match extract_token(request) {
        Some(token) => match auth.logout(&token) {
            Ok(_) => Response::ok(r#"{"status": "logged_out"}"#.to_string()),
            Err(e) => Response::error(500, &format!("{}", e)),
        },
        None => Response::error(401, "Missing token"),
    }
}

/// Handle token refresh requests in the v2 API.
pub fn handle_refresh(request: &Request) -> Response {
    let logger = get_logger("api.v2.auth");
    logger.info("V2 token refresh");
    let config = Config::load();
    match extract_token(request) {
        Some(token) => match refresh_token(&token, &config) {
            Ok(new_token) => Response::ok(format!(
                r#"{{"token": "{}", "expires_in": 3600}}"#,
                new_token.value
            )),
            Err(e) => Response::error(401, &e.message),
        },
        None => Response::error(401, "Missing token"),
    }
}
