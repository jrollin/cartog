use crate::*;

// ── normalize_symbol_name tests ──

#[test]
fn test_normalize_snake_case() {
    assert_eq!(normalize_symbol_name("validate_token"), "validate token");
    assert_eq!(
        normalize_symbol_name("get_current_user"),
        "get current user"
    );
    assert_eq!(normalize_symbol_name("_private_method"), "private method");
    assert_eq!(normalize_symbol_name("__init__"), "init");
}

#[test]
fn test_normalize_camel_case() {
    assert_eq!(normalize_symbol_name("validateToken"), "validate token");
    assert_eq!(normalize_symbol_name("getCurrentUser"), "get current user");
    assert_eq!(normalize_symbol_name("findByToken"), "find by token");
}

#[test]
fn test_normalize_pascal_case() {
    assert_eq!(
        normalize_symbol_name("DatabaseConnection"),
        "database connection"
    );
    assert_eq!(normalize_symbol_name("AuthService"), "auth service");
    assert_eq!(normalize_symbol_name("TokenError"), "token error");
}

#[test]
fn test_normalize_screaming_snake() {
    assert_eq!(normalize_symbol_name("TOKEN_EXPIRY"), "token expiry");
    assert_eq!(normalize_symbol_name("MAX_RETRY_COUNT"), "max retry count");
}

#[test]
fn test_normalize_acronyms() {
    assert_eq!(
        normalize_symbol_name("getHTTPResponse"),
        "get http response"
    );
    assert_eq!(normalize_symbol_name("parseJSON"), "parse json");
    assert_eq!(normalize_symbol_name("HTMLParser"), "html parser");
}

#[test]
fn test_normalize_single_word() {
    assert_eq!(normalize_symbol_name("validate"), "validate");
    assert_eq!(normalize_symbol_name("Token"), "token");
}

#[test]
fn test_normalize_empty_and_special() {
    assert_eq!(normalize_symbol_name(""), "");
    assert_eq!(normalize_symbol_name("_"), "");
    assert_eq!(normalize_symbol_name("___"), "");
}
