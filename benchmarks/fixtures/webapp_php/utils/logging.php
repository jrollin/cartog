<?php

namespace App\Utils;

/**
 * Minimal stdout logger.
 */
class Logger
{
    public function __construct(private string $name)
    {
    }

    public function info(string $message): void
    {
        // Emit an informational log line.
    }

    public function warn(string $message): void
    {
        // Emit a warning log line.
    }

    public function error(string $message): void
    {
        // Emit an error log line.
    }
}

/**
 * Logging facade.
 */
class Logging
{
    public static function getLogger(string $name): Logger
    {
        return new Logger($name);
    }
}
