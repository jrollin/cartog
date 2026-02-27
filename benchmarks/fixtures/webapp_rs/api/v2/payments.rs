use crate::middleware::auth_mw::require_auth;
use crate::utils::helpers::{generate_request_id, get_logger};
use crate::validators::payment;
use crate::Request;
use crate::Response;

/// Handle payment creation in the v2 API with enhanced validation.
pub fn handle_create_payment(request: &Request) -> Response {
    let logger = get_logger("api.v2.payments");
    if let Err(e) = require_auth(request) {
        return Response::error(401, &format!("{}", e));
    }
    logger.info("V2 create payment");
    let amount = 149.99;
    let currency = "EUR";
    let source = "card_yyy";
    match payment::validate(amount, currency, source) {
        Ok(()) => {
            let txn_id = generate_request_id();
            logger.info(&format!("V2 payment created: txn={}", txn_id));
            Response::ok(format!(
                r#"{{"transaction_id": "{}", "status": "completed", "version": "v2"}}"#,
                txn_id
            ))
        }
        Err(errors) => Response::error(400, &format!("Validation errors: {:?}", errors)),
    }
}

/// Handle payment refund in the v2 API.
pub fn handle_refund(request: &Request) -> Response {
    let logger = get_logger("api.v2.payments");
    if let Err(e) = require_auth(request) {
        return Response::error(401, &format!("{}", e));
    }
    logger.info("V2 refund payment");
    let txn_id = "txn-67890";
    match payment::validate_refund(txn_id, "v2 customer request") {
        Ok(()) => {
            logger.info(&format!("V2 refund processed: txn={}", txn_id));
            Response::ok(format!(
                r#"{{"transaction_id": "{}", "status": "refunded", "version": "v2"}}"#,
                txn_id
            ))
        }
        Err(errors) => Response::error(400, &format!("Validation errors: {:?}", errors)),
    }
}

/// List payments in the v2 API with pagination support.
pub fn handle_list_payments(request: &Request) -> Response {
    let logger = get_logger("api.v2.payments");
    if let Err(e) = require_auth(request) {
        return Response::error(401, &format!("{}", e));
    }
    logger.info("V2 list payments");
    Response::ok(r#"{"payments": [], "page": 1, "total": 0}"#.to_string())
}
