/** Notification management. */
import { getLogger, sanitizeInput } from '../../utils/helpers';
import { BaseService } from '../../auth/service';
import { DatabaseConnection } from '../../database/connection';
import { ValidationError } from '../../errors';

const logger = getLogger("services.notification");

/** Notification object. */
interface Notification {
    userId: string;
    channel: string;
    subject: string;
    body: string;
    status: string;
    createdAt: number;
}

/** Manages notifications. */
export class NotificationManager extends BaseService {
    private queue: Notification[] = [];

    constructor(private db: DatabaseConnection) {
        super("notification_manager");
    }

    /** Send a notification. */
    async send(userId: string, channel: string, subject: string, body: string): Promise<Notification> {
        this.requireInitialized();
        logger.info(`Queuing notification for ${userId} via ${channel}`);
        const validChannels = ["email", "sms", "push", "in_app"];
        if (!validChannels.includes(channel)) {
            throw new ValidationError(`Invalid channel: ${channel}`, "channel");
        }
        const notification: Notification = {
            userId,
            channel,
            subject: sanitizeInput(subject),
            body: sanitizeInput(body),
            status: "pending",
            createdAt: Date.now(),
        };
        this.queue.push(notification);
        await this.db.insert("notifications", notification as unknown as Record<string, unknown>);
        return notification;
    }

    /** Process the notification queue. */
    async processQueue(): Promise<{ sent: number; failed: number }> {
        logger.info(`Processing ${this.queue.length} notifications`);
        let sent = 0;
        let failed = 0;
        for (const n of this.queue) {
            if (n.status === "pending") {
                try {
                    n.status = "sent";
                    sent++;
                } catch {
                    n.status = "failed";
                    failed++;
                }
            }
        }
        this.queue = this.queue.filter(n => n.status === "pending");
        return { sent, failed };
    }
}
