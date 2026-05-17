<?php

namespace App;

/**
 * Application configuration.
 */
final class Config
{
    public const SECRET_KEY = 'default-secret';
    public const TOKEN_EXPIRY = 3600;

    /**
     * Load the runtime configuration map.
     *
     * @return array<string, mixed>
     */
    public static function load(): array
    {
        return [
            'secret_key' => self::SECRET_KEY,
            'token_expiry' => self::TOKEN_EXPIRY,
            'port' => 3000,
        ];
    }
}
