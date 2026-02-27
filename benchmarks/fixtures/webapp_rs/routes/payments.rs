use crate::utils::helpers::{get_logger, generate_request_id};
use crate::auth::middleware::auth_middleware;
use crate::validators::payment;
use crate::Request;
use crate::Response;

/// Handle payment creation requests.
pub fn create_payment_handler(request: Request) -> Response {
    let logger = get_logger("routes.payments");
    if let Err(e) = auth_middleware(&request) {
        return Response::error(401, &format!("{}", e));
    }
    logger.info("Creating payment");
    let amount = 99.99;
    let currency = "USD";
    let source = "card_xxx";
    match payment::validate(amount, currency, source) {
        Ok(()) => {
            let txn_id = generate_request_id();
            logger.info(&format!("Payment created: {}", txn_id));
            Response::ok(format!("{{"transaction_id": "{}"}}", txn_id))
        }
        Err(errors) => Response::error(400, &format!("{:?}", errors)),
    }
}

/// Handle payment refund requests.
pub fn refund_handler(request: Request) -> Response {
    let logger = get_logger("routes.payments");
    if let Err(e) = auth_middleware(&request) {
        return Response::error(401, &format!("{}", e));
    }
    logger.info("Processing refund");
    Response::ok("{"status": "refunded"}".to_string())
}

/// Handle listing payments for the authenticated user.
pub fn list_payments_handler(request: Request) -> Response {
    let logger = get_logger("routes.payments");
    if let Err(e) = auth_middleware(&request) {
        return Response::error(401, &format!("{}", e));
    }
    logger.info("Listing payments");
    Response::ok("[]".to_string())
}
