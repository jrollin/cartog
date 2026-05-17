<?php

namespace App\Tasks;

use App\Events\EventDispatcher;

use function App\Utils\get_logger;

/**
 * Background task that dispatches queued notification emails.
 */
class EmailTask
{
    public function __construct(private EventDispatcher $events)
    {
    }

    /**
     * @param list<array<string, mixed>> $queue
     */
    public function run(array $queue): int
    {
        get_logger('tasks.email')->info('Dispatching ' . count($queue) . ' emails');
        $sent = 0;
        foreach ($queue as $message) {
            $this->events->emit('email.sent', $message);
            $sent++;
        }

        return $sent;
    }
}
