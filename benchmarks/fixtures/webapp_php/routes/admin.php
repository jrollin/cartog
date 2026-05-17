<?php

namespace App\Routes;

use App\Auth\AdminService;

use function App\Auth\admin_required;
use function App\Auth\extract_token;

/**
 * Handle admin impersonation requests.
 *
 * @param array<string, mixed> $request
 *
 * @return array<string, mixed>
 */
function impersonate_route(array $request): array
{
    return admin_required($request, function (array $req) {
        $service = new AdminService($req['db']);
        $token = extract_token($req);
        $newToken = $service->impersonate($token, (int) $req['user_id']);

        return ['token' => $newToken, 'status' => 200];
    });
}

/**
 * Handle list-all-users requests.
 *
 * @param array<string, mixed> $request
 *
 * @return array<string, mixed>
 */
function list_users_route(array $request): array
{
    return admin_required($request, function (array $req) {
        $service = new AdminService($req['db']);
        $token = extract_token($req);
        $users = $service->listAllUsers($token);

        return ['users' => $users, 'status' => 200];
    });
}
