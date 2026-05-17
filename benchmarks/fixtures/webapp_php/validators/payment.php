<?php

namespace App\Validators;

use App\ValidationError;

/**
 * Validates payment input payloads.
 */
class PaymentValidator
{
    private const SUPPORTED_CURRENCIES = ['USD', 'EUR', 'GBP', 'JPY', 'CAD'];

    /**
     * Validate a payment payload.
     *
     * @param array<string, mixed> $payload
     */
    public function validate(array $payload): bool
    {
        $currency = (string) ($payload['currency'] ?? '');
        if (!in_array($currency, self::SUPPORTED_CURRENCIES, true)) {
            throw new ValidationError("Unsupported currency: {$currency}", 'currency');
        }
        $amount = (float) ($payload['amount'] ?? 0);
        if ($amount <= 0) {
            throw new ValidationError('Amount must be positive', 'amount');
        }

        return true;
    }
}
