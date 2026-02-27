/** User routes. */
import { getLogger, validateRequest, paginate } from '../utils/helpers';
import { DatabaseConnection } from '../database/connection';
import { UserQueries } from '../database/queries';
import { validate as validateUser } from '../validators/user';
import { NotFoundError } from '../errors';

const logger = getLogger("routes.users");

/** Get user. */
export async function getUserRoute(request: Record<string, unknown>, db: DatabaseConnection): Promise<Record<string, unknown>> {
    validateRequest(request);
    const params = request["params"] as Record<string, string>;
    const user = await db.findById("users", params?.id ?? "");
    if (!user) throw new NotFoundError("User", params?.id ?? "");
    return { status: 200, data: user };
}

/** List users. */
export async function listUsersRoute(request: Record<string, unknown>, db: DatabaseConnection): Promise<Record<string, unknown>> {
    validateRequest(request);
    const queries = new UserQueries(db);
    const users = await queries.findActiveUsers(200);
    return { status: 200, data: paginate(users) };
}

/** Update user. */
export async function updateUserRoute(request: Record<string, unknown>, db: DatabaseConnection): Promise<Record<string, unknown>> {
    validateRequest(request);
    const params = request["params"] as Record<string, string>;
    const body = request["body"] as Record<string, unknown>;
    const validated = validateUser(body);
    await db.update("users", params?.id ?? "", validated);
    return { status: 200, data: validated };
}
