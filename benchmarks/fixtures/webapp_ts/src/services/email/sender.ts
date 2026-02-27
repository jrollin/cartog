/** Email sending service. */
import { getLogger, sanitizeInput } from '../../utils/helpers';
import { CacheableService } from '../cacheable';
import { DatabaseConnection } from '../../database/connection';
import { ValidationError } from '../../errors';

const logger = getLogger("services.email");

const TEMPLATES: Record<string, string> = {
    welcome: "Welcome to our platform, {name}!",
    password_reset: "Reset your password: {link}",
    payment_receipt: "Payment of {amount} {currency} received. Txn: {txn_id}",
};

/** Email sender with templates. */
export class EmailSender extends CacheableService {
    private sentCount: number = 0;
    private failedCount: number = 0;

    constructor(private db: DatabaseConnection) {
        super("email_sender");
    }

    /** Send a single email. */
    async send(to: string, subject: string, body: string): Promise<boolean> {
        this.requireInitialized();
        logger.info(`Sending email to ${to}: ${subject}`);
        if (!to.includes("@")) throw new ValidationError("Invalid email", "to");
        try {
            await this.db.insert("notifications", { userId: "system", channel: "email", subject, body, status: "sent" });
            this.sentCount++;
            return true;
        } catch (e) {
            logger.error(`Email failed: ${e}`);
            this.failedCount++;
            return false;
        }
    }

    /** Send using a template. */
    async sendTemplate(to: string, templateName: string, context: Record<string, string>): Promise<boolean> {
        const template = TEMPLATES[templateName];
        if (!template) throw new ValidationError(`Unknown template: ${templateName}`, "template");
        let body = template;
        for (const [key, val] of Object.entries(context)) {
            body = body.replace(`{${key}}`, val);
        }
        const subject = `[App] ${templateName.replace("_", " ")}`;
        return this.send(to, subject, body);
    }

    /** Get sending stats. */
    stats(): { sent: number; failed: number } {
        return { sent: this.sentCount, failed: this.failedCount };
    }
}
