<?php

namespace App;

use RuntimeException;

/**
 * Base application error.
 */
class AppError extends RuntimeException
{
    public function __construct(string $message = 'Application error', int $code = 500)
    {
        parent::__construct($message, $code);
    }

    public function errorCode(): int
    {
        return $this->getCode();
    }
}

/**
 * Raised when input validation fails.
 */
class ValidationError extends AppError
{
    public function __construct(string $message = 'Validation failed', private ?string $field = null)
    {
        parent::__construct($message, 400);
    }

    public function field(): ?string
    {
        return $this->field;
    }
}

/**
 * Raised when a payment operation fails.
 */
class PaymentError extends AppError
{
    public function __construct(string $message = 'Payment failed', private ?string $transactionId = null)
    {
        parent::__construct($message, 402);
    }
}

/**
 * Raised when a resource is not found.
 */
class NotFoundError extends AppError
{
    public function __construct(private string $resource, private string $identifier)
    {
        parent::__construct("{$resource} with id '{$identifier}' not found", 404);
    }
}

/**
 * Raised when authentication fails.
 */
class AuthenticationError extends AppError
{
    public function __construct(string $message = 'Authentication required')
    {
        parent::__construct($message, 401);
    }
}

/**
 * Raised when a database operation fails.
 */
class DatabaseError extends AppError
{
    public function __construct(string $message = 'Database error', private ?string $query = null)
    {
        parent::__construct($message, 500);
    }
}
