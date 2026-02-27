/** Notification routes. */
import { getLogger, validateRequest } from '../utils/helpers';
import { DatabaseConnection } from '../database/connection';
import { NotificationManager } from '../services/notification/manager';

const logger = getLogger("routes.notifications");

/** Send notification. */
export async function sendNotificationRoute(request: Record<string, unknown>, db: DatabaseConnection): Promise<Record<string, unknown>> {
    validateRequest(request);
    const body = request["body"] as Record<string, unknown>;
    const manager = new NotificationManager(db);
    manager.initialize();
    const notification = await manager.send(
        body["user_id"] as string,
        (body["channel"] as string) ?? "email",
        body["subject"] as string,
        body["body"] as string,
    );
    return { status: 201, data: notification };
}
