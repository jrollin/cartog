<?php

namespace App\Middleware;

use function App\Utils\get_logger;

/**
 * Token-bucket rate limiting middleware.
 */
class RateLimitMiddleware
{
    /** @var array<string, int> */
    private array $counters = [];

    /** @param callable $app */
    public function __construct(private $app, private int $limit = 100)
    {
    }

    /**
     * @param array<string, mixed> $request
     *
     * @return mixed
     */
    public function call(array $request)
    {
        $key = $request['ip'] ?? 'anonymous';
        $this->counters[$key] = ($this->counters[$key] ?? 0) + 1;
        if ($this->counters[$key] > $this->limit) {
            get_logger('middleware.rate_limit')->warn("Rate limit exceeded for {$key}");

            return ['error' => 'Too many requests', 'status' => 429];
        }

        return ($this->app)($request);
    }
}
