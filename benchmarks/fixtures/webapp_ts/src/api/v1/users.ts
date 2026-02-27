/** API v1 user endpoints. */
import { getLogger, validateRequest, paginate } from '../../utils/helpers';
import { validate as validateUser } from '../../validators/user';
import { DatabaseConnection } from '../../database/connection';
import { UserQueries } from '../../database/queries';
import { NotFoundError } from '../../errors';

const logger = getLogger("api.v1.users");

/** Get user by ID. */
export async function handleGetUser(request: Record<string, unknown>, db: DatabaseConnection): Promise<Record<string, unknown>> {
    validateRequest(request);
    const params = request["params"] as Record<string, string>;
    const userId = params?.id ?? "";
    logger.info(`Getting user: ${userId}`);
    const user = await db.findById("users", userId);
    if (!user) throw new NotFoundError("User", userId);
    return { status: 200, data: user };
}

/** List users. */
export async function handleListUsers(request: Record<string, unknown>, db: DatabaseConnection): Promise<Record<string, unknown>> {
    validateRequest(request);
    const params = request["params"] as Record<string, string>;
    const page = parseInt(params?.page ?? "1", 10);
    const queries = new UserQueries(db);
    const users = await queries.findActiveUsers(200);
    return { status: 200, data: paginate(users, page) };
}

/** Update user. */
export async function handleUpdateUser(request: Record<string, unknown>, db: DatabaseConnection): Promise<Record<string, unknown>> {
    validateRequest(request);
    const params = request["params"] as Record<string, string>;
    const body = request["body"] as Record<string, unknown>;
    const validated = validateUser(body);
    await db.update("users", params?.id ?? "", validated);
    return { status: 200, data: validated };
}
