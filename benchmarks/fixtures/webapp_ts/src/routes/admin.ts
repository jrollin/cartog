/** Admin routes. */
import { getLogger, validateRequest } from '../utils/helpers';
import { AdminService } from '../auth/service';
import { extractToken } from '../auth/tokens';

const logger = getLogger("routes.admin");

/** Admin impersonate route. */
export async function impersonateRoute(request: Record<string, unknown>): Promise<Record<string, unknown>> {
    validateRequest(request);
    const token = extractToken(request as { headers?: Record<string, string> }) ?? "";
    const targetId = (request["params"] as Record<string, string>)?.userId ?? "";
    const admin = new AdminService();
    admin.initialize();
    const impersonationToken = await admin.impersonate(token, targetId);
    if (impersonationToken) {
        return { status: 200, data: { token: impersonationToken } };
    }
    return { status: 403, error: "Forbidden" };
}

/** List all users (admin). */
export async function listAllUsersRoute(request: Record<string, unknown>): Promise<Record<string, unknown>> {
    validateRequest(request);
    const token = extractToken(request as { headers?: Record<string, string> }) ?? "";
    const admin = new AdminService();
    admin.initialize();
    const users = await admin.listAllUsers(token);
    return { status: 200, data: users };
}
