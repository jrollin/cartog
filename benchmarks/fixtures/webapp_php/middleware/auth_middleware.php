<?php

namespace App\Middleware;

use App\AuthenticationError;
use App\Auth\TokenError;

use function App\Auth\extract_token;
use function App\Auth\validate_token;
use function App\Utils\get_logger;
use function App\Utils\validate_request;

/**
 * Authentication middleware for the request pipeline.
 */
class AuthMiddleware
{
    private const PUBLIC_PATHS = ['/health', '/login', '/register'];

    /** @param callable $app */
    public function __construct(private $app)
    {
    }

    /**
     * @param array<string, mixed> $request
     *
     * @return mixed
     */
    public function call(array $request)
    {
        validate_request($request);
        if (in_array($request['path'], self::PUBLIC_PATHS, true)) {
            return ($this->app)($request);
        }

        $token = extract_token($request);
        if ($token === null) {
            throw new AuthenticationError('Missing token');
        }

        try {
            $request['user'] = validate_token($token);
            $request['authenticated'] = true;

            return ($this->app)($request);
        } catch (TokenError) {
            get_logger('middleware.auth')->warn('Token validation failed');
            throw new AuthenticationError('Invalid token');
        }
    }
}
