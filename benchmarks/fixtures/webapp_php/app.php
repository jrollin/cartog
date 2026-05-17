<?php

namespace App;

use App\Utils\Logger;

use function App\Routes\handle_login;
use function App\Routes\impersonate_route;
use function App\Routes\list_users_route;
use function App\Routes\login_route;
use function App\Routes\refresh_route;
use function App\Utils\get_logger;

/**
 * Simple application container.
 */
class App
{
    private Logger $logger;

    /** @var array<string, callable> */
    private array $routes = [];

    /** @param array<string, mixed> $config */
    public function __construct(private array $config)
    {
        $this->logger = get_logger('app');
    }

    public function route(string $path, callable $handler): void
    {
        $this->routes[$path] = $handler;
    }

    /**
     * @param array<string, mixed> $request
     *
     * @return mixed
     */
    public function handleRequest(string $path, array $request)
    {
        $handler = $this->routes[$path] ?? null;
        if ($handler === null) {
            throw new \InvalidArgumentException("No route for {$path}");
        }

        return $handler($request);
    }
}

/**
 * Register all route handlers on the application.
 */
function register_routes(App $app): void
{
    $app->route('/login', 'App\Routes\login_route');
    $app->route('/session', 'App\Routes\handle_login');
    $app->route('/refresh', 'App\Routes\refresh_route');
    $app->route('/admin/impersonate', 'App\Routes\impersonate_route');
    $app->route('/admin/users', 'App\Routes\list_users_route');
}

/**
 * Create and configure the application.
 */
function create_app(): App
{
    $app = new App(Config::load());
    register_routes($app);
    get_logger('app')->info('Application created');

    return $app;
}
