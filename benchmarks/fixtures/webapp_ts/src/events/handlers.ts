/** Default event handlers. */
import { getLogger } from '../utils/helpers';
import { AppEvent, EventDispatcher } from './dispatcher';

const logger = getLogger("events.handlers");

export function onUserRegistered(event: AppEvent): void {
    logger.info(`User registered: ${event.data["email"]}`);
}

export function onLoginSuccess(event: AppEvent): void {
    logger.info(`Login success: ${event.data["email"]} from ${event.data["ip"]}`);
}

export function onLoginFailed(event: AppEvent): void {
    logger.info(`Login failed: ${event.data["email"]} from ${event.data["ip"]}`);
}

export function onPaymentCompleted(event: AppEvent): void {
    logger.info(`Payment completed: txn=${event.data["transactionId"]} amount=${event.data["amount"]}`);
}

export function onPaymentRefunded(event: AppEvent): void {
    logger.info(`Payment refunded: txn=${event.data["transactionId"]}`);
}

export function registerDefaultHandlers(dispatcher: EventDispatcher): void {
    dispatcher.on("auth.user_registered", onUserRegistered);
    dispatcher.on("auth.login_success", onLoginSuccess);
    dispatcher.on("auth.login_failed", onLoginFailed);
    dispatcher.on("payment.completed", onPaymentCompleted);
    dispatcher.on("payment.refunded", onPaymentRefunded);
    logger.info("Default handlers registered");
}
