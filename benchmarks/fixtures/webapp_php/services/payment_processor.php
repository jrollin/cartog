<?php

namespace App\Services;

use App\Auth\BaseService;
use App\Database\DatabaseConnection;
use App\Database\PaymentQueries;
use App\Events\EventDispatcher;
use App\NotFoundError;
use App\Validators\PaymentValidator;

use function App\Utils\generate_request_id;

/**
 * Payment processor with validation and audit trail.
 */
class PaymentProcessor extends BaseService
{
    private PaymentQueries $queries;
    private PaymentValidator $validator;

    public function __construct(DatabaseConnection $db, private EventDispatcher $events)
    {
        parent::__construct();
        $this->queries = new PaymentQueries($db);
        $this->validator = new PaymentValidator();
    }

    /**
     * Process a payment end-to-end.
     *
     * @return array<string, mixed>
     */
    public function processPayment(int $userId, float $amount, string $currency): array
    {
        $this->log("Processing payment: user={$userId}, amount={$amount} {$currency}");
        $this->validator->validate(['amount' => $amount, 'currency' => $currency]);
        $txnId = generate_request_id();
        $this->queries->createPayment($userId, $amount, $currency, $txnId);
        $this->queries->updateStatus($txnId, 'completed');
        $this->events->emit('payment.completed', ['transaction_id' => $txnId, 'user_id' => $userId]);

        return ['transaction_id' => $txnId, 'status' => 'completed'];
    }

    /**
     * Refund a payment by transaction id.
     *
     * @return array<string, mixed>
     */
    public function refund(string $transactionId, string $reason = ''): array
    {
        $this->log("Refunding: {$transactionId}");
        $payment = $this->queries->findByTransactionId($transactionId);
        if ($payment === null) {
            throw new NotFoundError('Payment', $transactionId);
        }
        $this->queries->updateStatus($transactionId, 'refunded');
        $this->events->emit('payment.refunded', ['transaction_id' => $transactionId, 'reason' => $reason]);

        return ['transaction_id' => $transactionId, 'status' => 'refunded'];
    }
}
