use crate::middleware::auth_mw::require_auth;
use crate::utils::helpers::{generate_request_id, get_logger};
use crate::validators::payment;
use crate::Request;
use crate::Response;

/// Handle payment creation in the v1 API.
pub fn handle_create_payment(request: &Request) -> Response {
    let logger = get_logger("api.v1.payments");
    if let Err(e) = require_auth(request) {
        return Response::error(401, &format!("{}", e));
    }
    logger.info("V1 create payment");
    let amount = 99.99;
    let currency = "USD";
    let source = "card_xxx";
    match payment::validate(amount, currency, source) {
        Ok(()) => {
            let txn_id = generate_request_id();
            logger.info(&format!("Payment created: txn={}", txn_id));
            Response::ok(format!(
                r#"{{"transaction_id": "{}", "status": "completed"}}"#,
                txn_id
            ))
        }
        Err(errors) => Response::error(400, &format!("Validation errors: {:?}", errors)),
    }
}

/// Handle payment refund in the v1 API.
pub fn handle_refund(request: &Request) -> Response {
    let logger = get_logger("api.v1.payments");
    if let Err(e) = require_auth(request) {
        return Response::error(401, &format!("{}", e));
    }
    logger.info("V1 refund payment");
    let txn_id = "txn-12345";
    match payment::validate_refund(txn_id, "customer request") {
        Ok(()) => {
            logger.info(&format!("Refund processed: txn={}", txn_id));
            Response::ok(format!(
                r#"{{"transaction_id": "{}", "status": "refunded"}}"#,
                txn_id
            ))
        }
        Err(errors) => Response::error(400, &format!("Validation errors: {:?}", errors)),
    }
}

/// List payments for the authenticated user.
pub fn handle_list_payments(request: &Request) -> Response {
    let logger = get_logger("api.v1.payments");
    if let Err(e) = require_auth(request) {
        return Response::error(401, &format!("{}", e));
    }
    logger.info("V1 list payments");
    Response::ok("[]".to_string())
}
