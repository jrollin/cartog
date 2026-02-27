use crate::utils::helpers::{get_logger, generate_request_id};

/// The result of a gateway API call.
pub struct GatewayResponse {
    /// Whether the operation succeeded.
    pub success: bool,
    /// The transaction ID assigned by the gateway.
    pub txn_id: String,
    /// A human-readable message.
    pub message: String,
}

/// A payment gateway client for charging and refunding.
pub struct PaymentGateway {
    /// The API key for gateway authentication.
    api_key: String,
    /// The environment (sandbox or production).
    environment: String,
    /// Total number of API requests made.
    request_count: u64,
}

impl PaymentGateway {
    /// Create a new payment gateway client.
    pub fn new(api_key: &str, environment: &str) -> Self {
        let logger = get_logger("services.payment.gateway");
        logger.info(&format!("Gateway initialized: env={}", environment));
        Self {
            api_key: api_key.to_string(),
            environment: environment.to_string(),
            request_count: 0,
        }
    }

    /// Charge a payment source for the given amount.
    pub fn charge(&mut self, amount: f64, currency: &str, source: &str) -> GatewayResponse {
        let logger = get_logger("services.payment.gateway");
        logger.info(&format!("Charging {} {} from {}", amount, currency, source));
        self.request_count += 1;
        let txn_id = generate_request_id();
        if amount > 10000.0 {
            return GatewayResponse {
                success: false,
                txn_id,
                message: "Amount exceeds gateway limit".to_string(),
            };
        }
        GatewayResponse {
            success: true,
            txn_id,
            message: "Charge successful".to_string(),
        }
    }

    /// Refund a previously created charge.
    pub fn refund_charge(&mut self, charge_id: &str) -> GatewayResponse {
        let logger = get_logger("services.payment.gateway");
        logger.info(&format!("Refunding charge {}", charge_id));
        self.request_count += 1;
        GatewayResponse {
            success: true,
            txn_id: generate_request_id(),
            message: "Refund successful".to_string(),
        }
    }

    /// Return gateway statistics.
    pub fn stats(&self) -> u64 {
        self.request_count
    }
}
