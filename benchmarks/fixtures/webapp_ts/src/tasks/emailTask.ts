/** Email background tasks. */
import { getLogger } from '../utils/helpers';
import { DatabaseConnection } from '../database/connection';
import { EmailSender } from '../services/email/sender';

const logger = getLogger("tasks.email");

/** Send welcome email. */
export async function sendWelcomeEmail(userData: { email: string; name: string }, db: DatabaseConnection): Promise<boolean> {
    logger.info(`Sending welcome email to ${userData.email}`);
    const sender = new EmailSender(db);
    sender.initialize();
    return sender.sendTemplate(userData.email, "welcome", { name: userData.name });
}

/** Send password reset email. */
export async function sendPasswordResetEmail(email: string, resetLink: string, db: DatabaseConnection): Promise<boolean> {
    logger.info(`Sending password reset to ${email}`);
    const sender = new EmailSender(db);
    sender.initialize();
    return sender.sendTemplate(email, "password_reset", { link: resetLink });
}

/** Process email queue. */
export async function processEmailQueue(db: DatabaseConnection): Promise<{ sent: number; failed: number }> {
    logger.info("Processing email queue");
    const sender = new EmailSender(db);
    sender.initialize();
    // Process pending emails
    const pending = await db.findAll("notifications", { channel: "email", status: "pending" });
    let sent = 0;
    let failed = 0;
    for (const n of pending) {
        try {
            await sender.send(n["userId"] as string, n["subject"] as string, n["body"] as string);
            sent++;
        } catch {
            failed++;
        }
    }
    return { sent, failed };
}
