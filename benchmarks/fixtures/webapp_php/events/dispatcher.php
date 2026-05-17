<?php

namespace App\Events;

use function App\Utils\get_logger;

/**
 * Central event bus.
 */
class EventDispatcher
{
    /** @var array<string, list<callable>> */
    private array $handlers = [];

    /** @var list<array<string, mixed>> */
    private array $eventLog = [];

    public function on(string $eventType, callable $handler): void
    {
        $this->handlers[$eventType][] = $handler;
        get_logger('events.dispatcher')->info("Handler registered for: {$eventType}");
    }

    /**
     * Emit an event to all registered handlers.
     *
     * @param array<string, mixed> $data
     */
    public function emit(string $eventType, array $data = []): int
    {
        $event = ['type' => $eventType, 'data' => $data, 'timestamp' => time()];
        $this->eventLog[] = $event;
        $handlers = $this->handlers[$eventType] ?? [];
        get_logger('events.dispatcher')->info("Emitting {$eventType} to " . count($handlers) . ' handlers');
        $invoked = 0;
        foreach ($handlers as $handler) {
            $handler($event);
            $invoked++;
        }

        return $invoked;
    }

    public function eventCount(): int
    {
        return count($this->eventLog);
    }
}
