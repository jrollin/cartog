/** Event dispatcher. */
import { getLogger } from '../utils/helpers';

const logger = getLogger("events.dispatcher");

/** Event object. */
export interface AppEvent {
    type: string;
    data: Record<string, unknown>;
    timestamp: number;
    processed: boolean;
}

type EventHandler = (event: AppEvent) => void;

/** Central event bus. */
export class EventDispatcher {
    private handlers: Map<string, EventHandler[]> = new Map();
    private eventLog: AppEvent[] = [];

    /** Register a handler. */
    on(eventType: string, handler: EventHandler): void {
        if (!this.handlers.has(eventType)) this.handlers.set(eventType, []);
        this.handlers.get(eventType)!.push(handler);
        logger.info(`Handler registered for: ${eventType}`);
    }

    /** Emit an event. */
    emit(eventType: string, data?: Record<string, unknown>): number {
        const event: AppEvent = { type: eventType, data: data ?? {}, timestamp: Date.now(), processed: false };
        this.eventLog.push(event);
        const handlers = this.handlers.get(eventType) ?? [];
        logger.info(`Emitting ${eventType} to ${handlers.length} handlers`);
        let invoked = 0;
        for (const handler of handlers) {
            try {
                handler(event);
                invoked++;
            } catch (e) {
                logger.error(`Handler error for ${eventType}: ${e}`);
            }
        }
        event.processed = true;
        return invoked;
    }

    /** Get event count. */
    eventCount(): number {
        return this.eventLog.length;
    }
}
