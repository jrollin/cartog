<?php

namespace App\Services;

use App\AuthenticationError;
use App\Auth\AuthService;
use App\Auth\BaseService;
use App\Auth\TokenError;
use App\Database\DatabaseConnection;
use App\Database\SessionQueries;
use App\Database\UserQueries;
use App\Events\EventDispatcher;
use App\ValidationError;

use function App\Auth\validate_token;
use function App\Utils\sanitize_input;

/**
 * Orchestrates authentication flows for the services layer.
 */
class AuthenticationService extends BaseService
{
    private AuthService $auth;
    private UserQueries $users;
    private SessionQueries $sessions;

    public function __construct(DatabaseConnection $db, private EventDispatcher $events)
    {
        parent::__construct();
        $this->auth = new AuthService($db);
        $this->users = new UserQueries($db);
        $this->sessions = new SessionQueries($db);
    }

    /**
     * Authenticate a user — main entry point for the login flow.
     *
     * @return array<string, mixed>
     */
    public function authenticate(string $email, string $password, string $ip = 'unknown'): array
    {
        $this->log("Authentication attempt for {$email}");
        $cleanEmail = sanitize_input($email);
        if ($cleanEmail === '') {
            throw new ValidationError('Email is required', 'email');
        }

        $token = $this->auth->login($cleanEmail, $password);
        if ($token === null) {
            $this->events->emit('auth.login_failed', ['email' => $cleanEmail, 'ip' => $ip]);
            throw new AuthenticationError('Invalid credentials');
        }
        $this->events->emit('auth.login_success', ['email' => $cleanEmail, 'ip' => $ip]);

        return ['token' => $token, 'email' => $cleanEmail];
    }

    /**
     * Verify a token and return the matching user record.
     *
     * @return array<string, mixed>|null
     */
    public function verifyToken(string $token): ?array
    {
        try {
            $user = validate_token($token);

            return $this->users->findByEmail($user->email);
        } catch (TokenError) {
            return null;
        }
    }

    public function doLogout(string $token): bool
    {
        $this->log('Processing logout');
        $session = $this->sessions->findActiveSession($token);
        if ($session !== null) {
            $this->sessions->expireSession((int) $session['id']);
            $this->events->emit('auth.logout', ['session_id' => $session['id']]);

            return true;
        }

        return false;
    }
}
