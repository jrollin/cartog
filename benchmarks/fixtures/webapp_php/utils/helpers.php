<?php

namespace App\Utils;

/**
 * Get a named logger instance.
 */
function get_logger(string $name): Logger
{
    return Logging::getLogger($name);
}

/**
 * Generate a unique request identifier.
 */
function generate_request_id(): string
{
    $ts = (int) (microtime(true) * 1000);
    $rand = bin2hex(random_bytes(4));

    return "req-{$ts}-{$rand}";
}

/**
 * Sanitize user input by removing control characters.
 */
function sanitize_input(?string $value): string
{
    if ($value === null || $value === '') {
        return '';
    }

    return trim(preg_replace('/[\x00-\x1f]/', '', $value));
}

/**
 * Validate that a request array has required fields.
 *
 * @param array<string, mixed> $request
 */
function validate_request(array $request): bool
{
    foreach (['method', 'path'] as $field) {
        if (!array_key_exists($field, $request)) {
            throw new \InvalidArgumentException("Missing required field: {$field}");
        }
    }

    return true;
}
