<?php

namespace App\Models;

/**
 * Payment lifecycle status.
 */
enum PaymentStatus: string
{
    case Pending = 'pending';
    case Processing = 'processing';
    case Completed = 'completed';
    case Failed = 'failed';
    case Refunded = 'refunded';
}

/**
 * Payment record.
 */
class Payment
{
    public function __construct(
        public readonly int $id,
        public readonly int $userId,
        public readonly float $amount,
        public readonly string $currency,
        public readonly string $transactionId,
        public PaymentStatus $status = PaymentStatus::Pending,
    ) {
    }

    public function complete(): void
    {
        $this->status = PaymentStatus::Completed;
    }

    public function fail(string $reason): void
    {
        $this->status = PaymentStatus::Failed;
    }

    public function completed(): bool
    {
        return $this->status === PaymentStatus::Completed;
    }
}
